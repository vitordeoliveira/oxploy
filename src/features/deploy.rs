//! Deploying a service: fetching its code, building it with cargo, keeping the binary
//! as a release, and running it in place of the previous one.
//!
//! Each deployment runs on a task of its own and writes what happens to its log, which
//! the Deployments tab shows as it grows. One build runs at a time on the machine, so
//! builds do not fight over its CPUs.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, BufReader};
use topcoat_asset::{Bundler, BundlerConfig, RawAsset};
use tokio::sync::Semaphore;
use toasty::Db;
use topcoat::context::{Cx, app_context};

use super::app_files::AppFiles;
use super::caddy::Caddy;
use super::databases::{self, ManagedDatabase};
use super::deployment::{Deployment, DeploymentStatus};
use super::domain::{self, Domain};
use super::environment::{PORT_VARIABLE, STORAGE_VARIABLE};
use super::archive;
use super::github::{GithubApp, Release, ReleaseFile};
use super::machine::Machine;
use super::project::{Project, now};
use super::runtime::{Exits, FromOxploy, Launch, Readiness, Runtime, app_variables, system_env};
use super::service::{BuildKind, Service, assign_port, release_file_matches};
use super::storage;
use super::usage::Usage;

/// How long a new release may take to start listening on its port.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How often oxploy looks for new GitHub releases of the services that deploy them on
/// their own.
const RELEASE_CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// How many releases before the live one are kept, to go back to.
const KEPT_RELEASES: usize = 2;

/// The variables of the panel's environment that cargo and rustup also need to find
/// their toolchains, besides asdf's own `ASDF_*` ones.
const TOOLCHAIN_ENV: [&str; 2] = ["CARGO_HOME", "RUSTUP_HOME"];

/// Where a service stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppStatus {
    /// It has never gone live.
    NotDeployed,
    /// A deployment is in progress.
    Deploying,
    Running,
    /// It should run but its process is not up, as after a crash.
    Restarting,
    /// The user stopped it.
    Stopped,
}

/// Why a deployment did not start.
#[derive(Debug)]
pub enum Refusal {
    /// The service has no repository and branch yet.
    NoSource,
    AlreadyDeploying,
}

/// Deploys services and runs their releases.
#[derive(Clone)]
pub struct Deployer {
    db: Db,
    caddy: Caddy,
    runtime: Arc<Runtime>,
    builds: Arc<Semaphore>,
    /// The services with a deployment in progress.
    deploying: Arc<Mutex<HashSet<u64>>>,
}

impl Deployer {
    /// Marks the deployments a previous run of the panel left unfinished as failed,
    /// then starts watching the apps that should run. A release that goes live tells
    /// `caddy`, so its domains reach it right away.
    pub async fn start(db: Db, caddy: Caddy) -> toasty::Result<Deployer> {
        for status in [DeploymentStatus::Queued, DeploymentStatus::Building, DeploymentStatus::Starting] {
            let unfinished = Deployment::filter(Deployment::fields().status().eq(status))
                .exec(&mut db.clone())
                .await?;
            for mut deployment in unfinished {
                deployment
                    .update()
                    .status(DeploymentStatus::Failed)
                    .finished_at(Some(now()))
                    .exec(&mut db.clone())
                    .await?;
                if let Some(service) = Service::filter_by_id(deployment.service_id)
                    .first()
                    .exec(&mut db.clone())
                    .await?
                {
                    let files = AppFiles::of(&service.app_name);
                    if let Ok(mut log) = DeployLog::open(&files, deployment.id) {
                        log.step("Failed: the panel stopped before this deployment finished");
                    }
                }
            }
        }
        let runtime = Arc::new(Runtime::new(db.clone()));
        runtime.clone().watch();
        let deployer = Deployer {
            db,
            caddy,
            runtime,
            builds: Arc::new(Semaphore::new(1)),
            deploying: Arc::default(),
        };
        tokio::spawn(deployer.clone().deploy_new_releases());
        Ok(deployer)
    }

    /// Returns the deployer registered on the router.
    pub fn get(cx: &Cx) -> &Deployer {
        app_context::<Deployer>(cx)
    }

    /// Where the service stands now.
    pub fn status(&self, service: &Service) -> AppStatus {
        if self.is_deploying(service.id) {
            AppStatus::Deploying
        } else if service.release_id.is_none() {
            AppStatus::NotDeployed
        } else if service.stopped_at.is_some() {
            AppStatus::Stopped
        } else if self.runtime.is_running(service.id) {
            AppStatus::Running
        } else {
            AppStatus::Restarting
        }
    }

    /// When the service's app started, while it runs.
    pub fn running_since(&self, service_id: u64) -> Option<std::time::SystemTime> {
        self.runtime.running_since(service_id)
    }

    pub fn is_deploying(&self, service_id: u64) -> bool {
        lock(&self.deploying).contains(&service_id)
    }

    /// How many deployments are queued or in progress.
    pub fn deploying(&self) -> usize {
        lock(&self.deploying).len()
    }

    /// The recent usage of each app.
    pub fn usage(&self) -> &Usage {
        self.runtime.usage()
    }

    /// How often the service's app exited on its own since a deployment or the user
    /// last started it.
    pub fn exits(&self, service_id: u64) -> Exits {
        self.runtime.exits(service_id)
    }

    /// Starts deploying the service on a task of its own, and returns its deployment.
    pub async fn deploy(&self, service: &Service) -> toasty::Result<Result<Deployment, Refusal>> {
        let has_source = service.github_app_id.is_some()
            && service.repository.is_some()
            && service.branch.is_some();
        if !has_source {
            return Ok(Err(Refusal::NoSource));
        }
        if !lock(&self.deploying).insert(service.id) {
            return Ok(Err(Refusal::AlreadyDeploying));
        }
        let created = toasty::create!(Deployment {
            service_id: service.id,
            status: DeploymentStatus::Queued,
            commit: None,
            commit_message: None,
            tag: None,
            binary: None,
            build_path: None,
            created_at: now(),
            finished_at: None,
        })
        .exec(&mut self.db.clone())
        .await;
        let deployment = match created {
            Ok(deployment) => deployment,
            Err(error) => {
                lock(&self.deploying).remove(&service.id);
                return Err(error);
            }
        };
        tokio::spawn(self.clone().run(deployment.id, service.id));
        Ok(Ok(deployment))
    }

    /// Stops the service and keeps it stopped until it is started or deployed again.
    pub async fn stop(&self, service: &mut Service) -> toasty::Result<()> {
        let _hold = self.runtime.hold(service.id);
        service
            .update()
            .stopped_at(Some(now()))
            .exec(&mut self.db.clone())
            .await?;
        self.runtime
            .stop(service.id, &AppFiles::of(&service.app_name))
            .await;
        Ok(())
    }

    /// Starts the service's release again, after it was stopped or to restart it.
    pub async fn restart(&self, service: &mut Service) -> toasty::Result<std::io::Result<()>> {
        let _hold = self.runtime.hold(service.id);
        service
            .update()
            .stopped_at(None)
            .exec(&mut self.db.clone())
            .await?;
        let files = AppFiles::of(&service.app_name);
        self.runtime.stop(service.id, &files).await;
        let Some(launch) = Launch::of(&self.db, service).await? else {
            return Ok(Ok(()));
        };
        Ok(self.runtime.start(&launch))
    }

    /// Puts a backup in place of a database: stops the service, backs up the current
    /// database, restores, and starts the service again if it should run. Returns what
    /// went wrong instead, such as a deployment in progress.
    pub async fn restore(
        &self,
        service: &Service,
        database: &str,
        backup: &str,
    ) -> toasty::Result<Result<(), String>> {
        let (database, backup) = (database.to_owned(), backup.to_owned());
        self.with_app_stopped(service, move |files| {
            // The database being replaced is kept, unless there is none yet.
            if storage::database_file(files, &database).is_some_and(|file| file.is_file()) {
                storage::back_up(files, &database, "-before-restore")?;
            }
            storage::restore(files, &database, &backup)
        })
        .await
    }

    /// Deletes a database oxploy manages, after backing it up: stops managing it, stops
    /// the service, deletes the file and its link in the running release, and starts
    /// the service again if it should run.
    pub async fn delete_database(
        &self,
        service: &Service,
        database: ManagedDatabase,
    ) -> toasty::Result<Result<(), String>> {
        let managed = ManagedDatabase::of_service_in(&self.db, service.id).await?;
        if let Some(problem) = databases::still_used(service, &managed, &database) {
            return Ok(Err(problem));
        }
        let (file, link) = (database.file.clone(), database.link.clone());
        // The link goes from the release that runs, or SQLite would make a new, empty
        // database through it.
        let work_dir = match Launch::of(&self.db, service).await? {
            Some(launch) => Some(launch.work_dir),
            None => None,
        };
        // Unmanaged first, so the start that follows makes no link to it.
        ManagedDatabase::filter_by_id(database.id)
            .delete()
            .exec(&mut self.db.clone())
            .await?;
        self.with_app_stopped(service, move |files| {
            if storage::database_file(files, &file).is_some_and(|path| path.is_file()) {
                storage::back_up(files, &file, "-before-delete")?;
            }
            if let Some(path) = storage::database_file(files, &file) {
                for path in storage::with_companions(&path) {
                    let _ = fs::remove_file(path);
                }
            }
            if let (Some(work_dir), Some(link)) = (&work_dir, &link) {
                let _ = fs::remove_file(work_dir.join(link));
            }
            Ok(())
        })
        .await
    }

    /// Stops the service while `change` works on its files, then starts it again if it
    /// should run. Returns what went wrong instead, such as a deployment in progress.
    async fn with_app_stopped<T: Send + 'static>(
        &self,
        service: &Service,
        change: impl FnOnce(&AppFiles) -> Result<T, storage::StorageError> + Send + 'static,
    ) -> toasty::Result<Result<T, String>> {
        if self.is_deploying(service.id) {
            return Ok(Err("a deployment is in progress; try again once it ends".to_owned()));
        }
        let files = AppFiles::of(&service.app_name);
        let _hold = self.runtime.hold(service.id);
        self.runtime.stop(service.id, &files).await;
        let changed = tokio::task::spawn_blocking({
            let files = files.clone();
            move || change(&files)
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|changed| changed.map_err(|error| error.to_string()));
        // Started again either way: a failed change leaves the files as they were.
        if service.release_id.is_some()
            && service.stopped_at.is_none()
            && let Some(launch) = Launch::of(&self.db, service).await?
            && let Err(error) = self.runtime.start(&launch)
        {
            eprintln!("oxploy: could not start {} again: {error}", service.app_name);
        }
        Ok(changed)
    }

    /// Stops the service and deletes its deployments and files, before the service
    /// itself is deleted.
    pub async fn forget(&self, service: &Service) -> toasty::Result<()> {
        let files = AppFiles::of(&service.app_name);
        let _hold = self.runtime.hold(service.id);
        self.runtime.stop(service.id, &files).await;
        self.runtime.forget(service.id);
        Deployment::filter_by_service_id(service.id)
            .delete()
            .exec(&mut self.db.clone())
            .await?;
        ManagedDatabase::disconnect_all(&self.db, service.id).await?;
        let _ = fs::remove_dir_all(files.root());
        Ok(())
    }

    /// Runs one deployment to the end and records how it went.
    async fn run(self, deployment_id: u64, service_id: u64) {
        let mut db = self.db.clone();
        let service = Service::filter_by_id(service_id).first().exec(&mut db).await;
        let (Ok(Some(service)), Ok(Some(mut deployment))) = (
            service,
            Deployment::filter_by_id(deployment_id).first().exec(&mut db).await,
        ) else {
            lock(&self.deploying).remove(&service_id);
            return;
        };
        let files = AppFiles::of(&service.app_name);
        let outcome = match DeployLog::open(&files, deployment_id) {
            Ok(mut log) => {
                let outcome = self.attempt(&mut deployment, &service, &files, &mut log).await;
                match &outcome {
                    Ok(()) => log.step("Deployed"),
                    Err(problem) => log.step(&format!("Failed: {problem}")),
                }
                outcome
            }
            Err(error) => Err(format!("the log could not be written: {error}")),
        };
        let status = match outcome {
            Ok(()) => DeploymentStatus::Succeeded,
            Err(_) => {
                // A failed build's code is not worth its space; its log stays.
                let _ = fs::remove_dir_all(files.release(deployment_id));
                DeploymentStatus::Failed
            }
        };
        let saved = deployment
            .update()
            .status(status)
            .finished_at(Some(now()))
            .exec(&mut db)
            .await;
        if let Err(error) = saved {
            eprintln!("oxploy: could not save deployment #{deployment_id}: {error}");
        }
        lock(&self.deploying).remove(&service_id);
    }

    /// Fetches, builds and starts the service, writing each step to `log`.
    async fn attempt(
        &self,
        deployment: &mut Deployment,
        service: &Service,
        files: &AppFiles,
        log: &mut DeployLog,
    ) -> Result<(), String> {
        let mut db = self.db.clone();
        let saved = |error: toasty::Error| format!("the database could not be updated: {error}");

        let kind = service.builds_with();
        // A GitHub release was built elsewhere, so only builds wait for each other.
        let _build = match kind {
            BuildKind::Release => None,
            BuildKind::Cargo | BuildKind::Script => {
                Some(match self.builds.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        log.step("Waiting for another build to finish");
                        self.builds
                            .clone()
                            .acquire_owned()
                            .await
                            .map_err(|error| error.to_string())?
                    }
                })
            }
        };
        deployment
            .update()
            .status(DeploymentStatus::Building)
            .exec(&mut db)
            .await
            .map_err(saved)?;

        // Fetch.
        let (Some(app_id), Some(repository), Some(branch)) =
            (service.github_app_id, &service.repository, &service.branch)
        else {
            return Err("choose a repository and a branch in the General tab".to_owned());
        };
        let app = self.github_app(service, app_id).await.map_err(saved)?.ok_or(
            "the GitHub account was removed; choose another one in the General tab",
        )?;
        let source = files.source(deployment.id);
        let _ = fs::remove_dir_all(&source);
        fs::create_dir_all(files.release(deployment.id)).map_err(|error| error.to_string())?;
        // The binary of a release, which needs no build.
        let released = match kind {
            BuildKind::Release => {
                let (release, file) = find_release(&app, repository, service, log).await?;
                // Kept before the download, so a release whose download fails counts as
                // tried, and new releases do not try it again.
                let title = release
                    .name
                    .clone()
                    .filter(|name| !name.trim().is_empty() && *name != release.tag_name);
                deployment
                    .update()
                    .tag(Some(release.tag_name.clone()))
                    .commit_message(title)
                    .exec(&mut db)
                    .await
                    .map_err(saved)?;
                let download = ReleaseDownload {
                    app: &app,
                    repository,
                    release: &release,
                    file: &file,
                };
                Some(fetch_release(&download, service, files, deployment.id, log).await?)
            }
            BuildKind::Cargo | BuildKind::Script => {
                let (commit, message) = fetch_branch(&app, repository, branch, &source, log).await?;
                deployment
                    .update()
                    .commit(commit)
                    .commit_message(message)
                    .build_path(service.build_path.clone())
                    .exec(&mut db)
                    .await
                    .map_err(saved)?;
                None
            }
        };

        // Build.
        let work_dir = match (&released, &service.build_path) {
            (None, Some(path)) => source.join(path),
            _ => source.clone(),
        };
        if !work_dir.is_dir() {
            let path = service.build_path.as_deref().unwrap_or("");
            return Err(format!("there is no folder /{path} in the repository"));
        }
        // The port comes first, so the variables can use `${PORT}` in builds too.
        let port = match service.port {
            Some(port) => port,
            None => assign_port(&self.db, service.id)
                .await
                .map_err(saved)?
                .ok_or("there is no free port left for apps")?,
        };
        let databases = ManagedDatabase::of_service_in(&self.db, service.id)
            .await
            .map_err(saved)?;
        let oxploy = FromOxploy::of(&self.db, service).await.map_err(saved)?;
        let variables = app_variables(service, files, port, &databases, &oxploy);
        let build = Build {
            service,
            files,
            deployment_id: deployment.id,
            source: &source,
            work_dir: &work_dir,
            variables: &variables,
        };
        let name = match (released, kind) {
            (Some(binary), _) => binary,
            (None, BuildKind::Cargo) => build_with_cargo(&build, log).await?,
            (None, BuildKind::Script | BuildKind::Release) => build_with_script(&build, log).await?,
        };
        let binary = files.binary(deployment.id, &name);
        // One built for another machine would only fail to start, as an app that exits.
        let this = Machine::this();
        if let Ok(Some(machine)) = Machine::of_binary(&binary)
            && !machine.runs_on(this)
        {
            let advice = match kind {
                BuildKind::Release => "choose the release's file for this server under Build in the General tab",
                BuildKind::Cargo | BuildKind::Script => "build it for this server",
            };
            return Err(format!("{name} is built for {machine}, and this server is {this}; {advice}"));
        }
        deployment
            .update()
            .binary(Some(name))
            .exec(&mut db)
            .await
            .map_err(saved)?;
        let linked = databases::link_all(&work_dir, files, &databases)
            .map_err(|error| format!("the databases could not be linked: {error}"))?;
        for link in linked {
            log.line(&format!("Linked {link} to the storage"));
        }
        if let Some(script) = &service.after_build {
            run_after_build(&build, script, &binary, port, log).await?;
        }

        // Start.
        deployment
            .update()
            .status(DeploymentStatus::Starting)
            .exec(&mut db)
            .await
            .map_err(saved)?;
        // The service as it is now, in case the user stopped it during the build.
        let mut service = Service::filter_by_id(service.id)
            .first()
            .exec(&mut db)
            .await
            .map_err(saved)?
            .ok_or("the service was deleted")?;
        let launch = Launch {
            service_id: service.id,
            files: files.clone(),
            release_id: deployment.id,
            binary,
            work_dir,
            port,
            variables,
            databases,
        };
        let _hold = self.runtime.hold(service.id);
        let previous = match service.stopped_at {
            None => Launch::of(&self.db, &service).await.map_err(saved)?,
            Some(_) => None,
        };
        self.runtime.stop(service.id, files).await;
        if let Some(previous) = &previous {
            log.line(&format!("Stopped release #{}", previous.release_id));
        }
        log.step(&format!("Starting on 127.0.0.1:{port}"));
        let started = match self.runtime.start(&launch) {
            Ok(()) => match self.runtime.wait_until_ready(service.id, port, READY_TIMEOUT).await {
                Readiness::Listening => {
                    log.line(&format!("Listening on 127.0.0.1:{port}"));
                    Ok(())
                }
                Readiness::NotListening => {
                    log.line(&format!(
                        "Running, but nothing answers on 127.0.0.1:{port} after {}s. A web app must listen there: read PORT, and bind 127.0.0.1.",
                        READY_TIMEOUT.as_secs()
                    ));
                    Ok(())
                }
                Readiness::Exited(how) => Err(format!(
                    "the app exited while starting ({how}); what it printed is in the Logs tab"
                )),
            },
            Err(error) => Err(format!("the binary could not start: {error}")),
        };
        if let Err(problem) = started {
            self.runtime.stop(service.id, files).await;
            if let Some(previous) = previous
                && self.runtime.start(&previous).is_ok()
            {
                log.line(&format!("Started release #{} again", previous.release_id));
            }
            return Err(problem);
        }
        service
            .update()
            .release_id(Some(deployment.id))
            .stopped_at(None)
            .exec(&mut db)
            .await
            .map_err(saved)?;
        // The release is live either way, so a domain that cannot be given only shows
        // in the log.
        if let Err(error) = self.offer_domain(&mut service, log).await {
            log.line(&format!("No domain was added: the database could not be updated: {error}"));
        }
        self.caddy.changed();
        prune(files, deployment.id);
        Ok(())
    }

    /// Gives the service a domain at the server's address, if it has none the first
    /// time it goes live with an address to give, so the app opens from the internet
    /// without setting one up. See [`Service::domain_offered_at`].
    async fn offer_domain(&self, service: &mut Service, log: &mut DeployLog) -> toasty::Result<()> {
        if service.domain_offered_at.is_some() {
            return Ok(());
        }
        // Without one, a later deployment tries again.
        let Some(address) = domain::generated_address(&self.caddy.ports().public_ips) else {
            return Ok(());
        };
        if !Domain::any_of_service(&self.db, service.id).await? {
            match Domain::add_generated(&self.db, service, address).await? {
                Ok(domain) => log.line(&format!("Added the domain {}", domain.url())),
                Err(problem) => {
                    log.line(&format!("No domain was added: {problem}"));
                    return Ok(());
                }
            }
        }
        service
            .update()
            .domain_offered_at(Some(now()))
            .exec(&mut self.db.clone())
            .await
    }

    /// The service's GitHub App, if it still exists in the service's organization.
    /// Deploys, every [`RELEASE_CHECK_INTERVAL`], the services that deploy new GitHub
    /// releases on their own, once their repository has a release they have not tried.
    async fn deploy_new_releases(self) {
        loop {
            tokio::time::sleep(RELEASE_CHECK_INTERVAL).await;
            let services = match Service::all().exec(&mut self.db.clone()).await {
                Ok(services) => services,
                Err(error) => {
                    eprintln!("oxploy: the services could not be read: {error}");
                    continue;
                }
            };
            for service in services {
                let watched = service.builds_with() == BuildKind::Release
                    && service.deploys_new_releases == Some(true)
                    && service.stopped_at.is_none();
                if !watched || self.is_deploying(service.id) {
                    continue;
                }
                if let Err(error) = self.deploy_if_released(&service).await {
                    eprintln!("oxploy: could not look for a release of {}: {error}", service.app_name);
                }
            }
        }
    }

    /// Deploys the service if its repository's latest release is not the one it last
    /// tried, and has the service's file. A release that failed is not tried again on
    /// its own.
    async fn deploy_if_released(&self, service: &Service) -> toasty::Result<()> {
        let (Some(app_id), Some(repository), Some(pattern)) =
            (service.github_app_id, &service.repository, &service.github_asset)
        else {
            return Ok(());
        };
        let Some(app) = self.github_app(service, app_id).await? else {
            return Ok(());
        };
        // When GitHub cannot be reached, the next look tries again.
        let Ok(Some(release)) = app.latest_release(repository).await else {
            return Ok(());
        };
        let last = Deployment::filter_by_service_id(service.id)
            .order_by(Deployment::fields().id().desc())
            .first()
            .exec(&mut self.db.clone())
            .await?;
        let tried = last.is_some_and(|last| last.tag.as_deref() == Some(release.tag_name.as_str()));
        // A release still getting its files waits for the next look.
        if tried || release_file(&release, pattern).is_err() {
            return Ok(());
        }
        let _ = self.deploy(service).await?;
        Ok(())
    }

    async fn github_app(&self, service: &Service, app_id: u64) -> toasty::Result<Option<GithubApp>> {
        let mut db = self.db.clone();
        let Some(project) = Project::filter_by_id(service.project_id).first().exec(&mut db).await?
        else {
            return Ok(None);
        };
        let app = GithubApp::filter_by_id(app_id).first().exec(&mut db).await?;
        Ok(app.filter(|app| app.organization_id == project.organization_id))
    }

}

/// What one deployment's build works with.
struct Build<'a> {
    service: &'a Service,
    files: &'a AppFiles,
    deployment_id: u64,
    /// The repository's code.
    source: &'a Path,
    /// The build path in the code, where the build runs.
    work_dir: &'a Path,
    variables: &'a [(String, String)],
}

impl Build<'_> {
    /// A command that runs in the build path with the variables builds get: the system's
    /// and the toolchains', the service's own, and cargo's cache in the app's target
    /// folder. The panel's own variables stay out.
    fn command(&self, program: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(program);
        command
            .current_dir(self.work_dir)
            .env_clear()
            .envs(system_env())
            .envs(TOOLCHAIN_ENV.iter().filter_map(|name| Some((*name, std::env::var(name).ok()?))))
            .envs(std::env::vars().filter(|(name, _)| name.starts_with("ASDF_")))
            .envs(self.variables.iter().map(|(key, value)| (key, value)))
            .env("CARGO_TARGET_DIR", self.files.target())
            .env("CARGO_TERM_COLOR", "never");
        command
    }
}

/// Fetches the branch's latest commit into `source`, and returns the commit and the first
/// line of its message.
async fn fetch_branch(
    app: &GithubApp,
    repository: &str,
    branch: &str,
    source: &Path,
    log: &mut DeployLog,
) -> Result<(Option<String>, Option<String>), String> {
    log.step(&format!("Fetching {repository} at {branch}"));
    let token = app
        .installation_token()
        .await
        .map_err(|error| error.to_string())?;
    let mut clone = tokio::process::Command::new("git");
    clone
        .args(["clone", "--depth", "1", "--single-branch", "--no-tags", "--branch"])
        .arg(branch)
        .arg("--")
        .arg(format!("https://github.com/{repository}.git"))
        .arg(source)
        .env_clear()
        .envs(system_env())
        .envs(git_auth(&token))
        .env("GIT_TERMINAL_PROMPT", "0");
    let status = run_logged(clone, log, |line| Some(line.to_owned()))
        .await
        .map_err(|error| format!("git could not run: {error}"))?;
    if !status.success() {
        return Err("git could not fetch the code".to_owned());
    }
    let commit = git_output(source, &["rev-parse", "HEAD"]).await;
    let message = git_output(source, &["log", "-1", "--format=%s"]).await;
    if let Some(commit) = &commit {
        let short = commit.get(..7).unwrap_or(commit);
        log.line(&format!("Commit {short} {}", message.as_deref().unwrap_or("")));
    }
    Ok((commit, message))
}

/// Finds the repository's latest GitHub release, and the service's file of it.
async fn find_release(
    app: &GithubApp,
    repository: &str,
    service: &Service,
    log: &mut DeployLog,
) -> Result<(Release, ReleaseFile), String> {
    let pattern = service
        .github_asset
        .as_deref()
        .ok_or("choose the release's file under Build in the General tab")?;
    log.step(&format!("Fetching the latest release of {repository}"));
    let release = app
        .latest_release(repository)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("{repository} has no release yet"))?;
    match release.name.as_deref().map(str::trim) {
        Some(title) if !title.is_empty() && title != release.tag_name => {
            log.line(&format!("Release {}, {title}", release.tag_name));
        }
        _ => log.line(&format!("Release {}", release.tag_name)),
    }
    let file = release_file(&release, pattern)?.clone();
    Ok((release, file))
}

/// A file of a GitHub release, to download for a deployment.
#[derive(Clone, Copy)]
struct ReleaseDownload<'a> {
    app: &'a GithubApp,
    repository: &'a str,
    release: &'a Release,
    file: &'a ReleaseFile,
}

/// Downloads the file, checks it against the release's checksum of it when there is
/// one, and puts it in the release's `source`, unpacked when it is an archive. The
/// release's `bin` folder is a link to `source`, so the binary runs with the files that
/// came beside it, as a built one runs beside its assets. Returns the binary to start,
/// from `bin`.
async fn fetch_release(
    download_of: &ReleaseDownload<'_>,
    service: &Service,
    files: &AppFiles,
    deployment_id: u64,
    log: &mut DeployLog,
) -> Result<String, String> {
    let ReleaseDownload {
        app,
        repository,
        release,
        file,
    } = *download_of;
    let folder = files.release(deployment_id);
    let download = folder.join("download");
    log.step(&format!("Downloading {} ({})", file.name, megabytes(file.size)));
    app.download_release_file(repository, file, &download)
        .await
        .map_err(|error| format!("the download failed: {error}"))?;

    let checksum_name = format!("{}.sha256", file.name);
    let checksum_file = release
        .assets
        .iter()
        .find(|asset| asset.name == checksum_name && asset.state == "uploaded");
    match checksum_file {
        Some(checksum_file) => {
            let checksum = folder.join("download.sha256");
            app.download_release_file(repository, checksum_file, &checksum)
                .await
                .map_err(|error| format!("the download of {checksum_name} failed: {error}"))?;
            let expected = fs::read_to_string(&checksum)
                .ok()
                .as_deref()
                .and_then(archive::checksum_in)
                .ok_or_else(|| format!("{checksum_name} has no checksum in it"))?;
            let _ = fs::remove_file(&checksum);
            let path = download.clone();
            let actual = tokio::task::spawn_blocking(move || archive::sha256(&path))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| format!("the download could not be read: {error}"))?;
            if actual != expected {
                return Err(format!("the download does not match {checksum_name}"));
            }
            log.line(&format!("Checked it against {checksum_name}"));
        }
        None => log.line(&format!(
            "The release has no {checksum_name} to check the download against"
        )),
    }

    let source = files.source(deployment_id);
    fs::create_dir_all(&source).map_err(|error| error.to_string())?;
    let binary = if archive::is_archive(&file.name) {
        let inside = service
            .github_asset_binary
            .clone()
            .ok_or("name the binary in the archive under Build in the General tab")?;
        let (archive_path, into) = (download.clone(), source.clone());
        tokio::task::spawn_blocking(move || archive::unpack(&archive_path, &into))
            .await
            .map_err(|error| error.to_string())??;
        log.line(&format!("Unpacked {}", file.name));
        let _ = fs::remove_file(&download);
        inside
    } else {
        // GitHub keeps release files' names free of slashes.
        fs::rename(&download, source.join(&file.name)).map_err(|error| error.to_string())?;
        file.name.clone()
    };
    let path = source.join(&binary);
    if !path.is_file() {
        return Err(format!("{} has no file {binary}", file.name));
    }
    // Runnable, whatever the release kept of its mode.
    let mut permissions = fs::metadata(&path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    fs::set_permissions(&path, permissions).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink("source", folder.join("bin")).map_err(|error| error.to_string())?;
    Ok(binary)
}

/// The file of `release` that `pattern` names, among those uploaded whole. Checksum
/// files only match a pattern for one.
fn release_file<'a>(release: &'a Release, pattern: &str) -> Result<&'a ReleaseFile, String> {
    let matching: Vec<&ReleaseFile> = release
        .assets
        .iter()
        .filter(|file| file.state == "uploaded")
        .filter(|file| !file.name.ends_with(".sha256") || pattern.ends_with(".sha256"))
        .filter(|file| release_file_matches(pattern, &file.name))
        .collect();
    let tag = &release.tag_name;
    match matching.as_slice() {
        [file] => Ok(file),
        [] if release.assets.is_empty() => Err(format!("release {tag} has no files")),
        [] => {
            let names: Vec<&str> = release.assets.iter().map(|file| file.name.as_str()).collect();
            Err(format!(
                "release {tag} has no file named {pattern}; its files are {}",
                names.join(", ")
            ))
        }
        several => {
            let names: Vec<&str> = several.iter().map(|file| file.name.as_str()).collect();
            Err(format!(
                "several files of release {tag} match {pattern}: {}; name one of them",
                names.join(", ")
            ))
        }
    }
}

/// A size in bytes as megabytes, such as `12.3 MB`.
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Builds with `cargo build --release`, keeps the binary in the release, and bundles
/// the assets of a Topcoat app. Returns the binary's name.
async fn build_with_cargo(build: &Build<'_>, log: &mut DeployLog) -> Result<String, String> {
    let service = build.service;
    if !build.work_dir.join("Cargo.toml").is_file() {
        let path = service.build_path.as_deref().unwrap_or("");
        return Err(format!("there is no Cargo.toml in /{path}"));
    }
    log.step(&format!("Building with {}", service.build_command()));
    let mut cargo = build.command("cargo");
    cargo.args(["build", "--release", "--message-format=json-render-diagnostics"]);
    if let Some(binary) = &service.cargo_bin {
        cargo.args(["--bin", binary]);
    }
    if let Some(features) = &service.cargo_features {
        cargo.args(["--features", features]);
    }
    if let Some(version) = &service.rust_version {
        cargo.env("RUSTUP_TOOLCHAIN", version);
    }
    let mut binaries = Vec::new();
    let status = run_logged(cargo, log, |line| match built_binary(line) {
        Some(Some(binary)) => {
            binaries.push(binary);
            None
        }
        Some(None) => None,
        None => Some(line.to_owned()),
    })
    .await
    .map_err(|error| format!("cargo could not run: {error}"))?;
    if !status.success() {
        return Err("cargo could not build the app".to_owned());
    }
    let (name, built) = match binaries.as_slice() {
        [] => return Err("the build made no binary; is this a library?".to_owned()),
        [binary] => binary.clone(),
        several => {
            let names: Vec<_> = several.iter().map(|(name, _)| name.as_str()).collect();
            return Err(format!(
                "the build made several binaries ({}); choose one under Build in the General tab",
                names.join(", ")
            ));
        }
    };
    let binary = build.files.binary(build.deployment_id, &name);
    if let Some(bin) = binary.parent() {
        fs::create_dir_all(bin).map_err(|error| error.to_string())?;
    }
    fs::copy(&built, &binary).map_err(|error| format!("the binary could not be kept: {error}"))?;
    log.line(&format!("Built {name}"));

    // A Topcoat app loads the files it declares with `asset!` from a bundle next to its
    // binary, even when it declares none.
    if let Some(version) = locked_version(build.work_dir, build.source, "topcoat-asset") {
        if !compatible(&version, TOPCOAT_ASSET_VERSION) {
            log.line(&format!(
                "warning: this app uses topcoat-asset {version}, but oxploy bundles assets for {TOPCOAT_ASSET_VERSION}. If the app cannot find its assets, oxploy needs an update."
            ));
        }
        match bundle_assets(binary, build.files.asset_cache()).await? {
            1 => log.line("Bundled 1 asset"),
            count => log.line(&format!("Bundled {count} assets")),
        }
    }
    Ok(name)
}

/// Runs the service's build script with `sh -e`, so the first command that fails
/// stops it. The script leaves the binary to run, and any files it needs, in the
/// release's `bin` folder, which it finds in `OXPLOY_OUTPUT_DIR`. Returns the binary's
/// name.
async fn build_with_script(build: &Build<'_>, log: &mut DeployLog) -> Result<String, String> {
    let (Some(script), Some(name)) = (&build.service.build_script, &build.service.script_binary)
    else {
        return Err("write a build script under Build in the General tab".to_owned());
    };
    let output = build.files.release(build.deployment_id).join("bin");
    fs::create_dir_all(&output).map_err(|error| error.to_string())?;
    log.step("Running the build script");
    let mut shell = build.command("sh");
    shell
        .args(["-e", "-c", script])
        .env("OXPLOY_OUTPUT_DIR", &output);
    let status = run_logged(shell, log, |line| Some(line.to_owned()))
        .await
        .map_err(|error| format!("the build script could not run: {error}"))?;
    if !status.success() {
        return Err(format!("the build script failed ({status})"));
    }
    let executable = fs::metadata(output.join(name))
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0);
    if !executable {
        return Err(format!(
            "the build script left no executable {name} in $OXPLOY_OUTPUT_DIR"
        ));
    }
    log.line(&format!("Built {name}"));
    Ok(name.clone())
}

/// Runs the service's script after the build, before the new release starts, as the
/// app will run: in the same folder, with the same variables, and with its binary on
/// the `PATH`. The release that runs keeps running if it fails.
async fn run_after_build(
    build: &Build<'_>,
    script: &str,
    binary: &Path,
    port: u64,
    log: &mut DeployLog,
) -> Result<(), String> {
    let storage = build.files.storage();
    fs::create_dir_all(&storage).map_err(|error| error.to_string())?;
    let bin = binary.parent().unwrap_or(binary);
    let path = match std::env::var("PATH") {
        Ok(path) => format!("{}:{path}", bin.display()),
        Err(_) => bin.display().to_string(),
    };
    log.step("Running the script after the build");
    let mut shell = build.command("sh");
    shell
        .args(["-e", "-c", script])
        .env("PATH", path)
        .env(PORT_VARIABLE, port.to_string())
        .env(STORAGE_VARIABLE, &storage);
    let status = run_logged(shell, log, |line| Some(line.to_owned()))
        .await
        .map_err(|error| format!("the script after the build could not run: {error}"))?;
    if !status.success() {
        // `./api`, as one runs a binary next to it, finds nothing where the app runs
        // when the binary was built there.
        let name = binary.file_name().map(|name| name.to_string_lossy().into_owned());
        if let Some(name) = name
            && script.contains(&format!("./{name}"))
            && !build.work_dir.join(&name).exists()
        {
            log.line(&format!(
                "The script runs where the app runs, and the new binary is on its PATH: call it as {name}, without ./"
            ));
        }
        return Err(format!("the script after the build failed ({status})"));
    }
    Ok(())
}

/// A deployment's log file.
struct DeployLog {
    file: File,
}

impl DeployLog {
    fn open(files: &AppFiles, deployment_id: u64) -> std::io::Result<DeployLog> {
        let path = files.deploy_log(deployment_id);
        if let Some(logs) = path.parent() {
            fs::create_dir_all(logs)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(DeployLog { file })
    }

    fn line(&mut self, text: &str) {
        let _ = writeln!(self.file, "{text}");
    }

    /// A line that starts a step, which the log shows as a heading.
    fn step(&mut self, text: &str) {
        self.line(&format!("==> {text}"));
    }
}

/// Runs `command`, writing what it prints to `log`. Lines on standard output go
/// through `stdout` first, which returns what to write, if anything.
async fn run_logged(
    mut command: tokio::process::Command,
    log: &mut DeployLog,
    mut stdout: impl FnMut(&str) -> Option<String>,
) -> std::io::Result<ExitStatus> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut out = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
    let mut err = BufReader::new(child.stderr.take().expect("stderr is piped")).lines();
    let (mut out_done, mut err_done) = (false, false);
    while !(out_done && err_done) {
        tokio::select! {
            line = out.next_line(), if !out_done => match line? {
                Some(line) => {
                    if let Some(text) = stdout(&line) {
                        log.line(&text);
                    }
                }
                None => out_done = true,
            },
            line = err.next_line(), if !err_done => match line? {
                Some(line) => log.line(&line),
                None => err_done = true,
            },
        }
    }
    child.wait().await
}

/// Runs git in `dir` and returns the first line it prints, if it succeeds.
async fn git_output(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_clear()
        .envs(system_env())
        .output()
        .await
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    output
        .status
        .success()
        .then(|| text.lines().next().unwrap_or("").to_owned())
}

/// Variables that let git fetch from GitHub with an installation token. They go in the
/// environment, so the token is neither in git's arguments nor saved in the clone.
fn git_auth(token: &str) -> [(&'static str, String); 3] {
    let credentials =
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    [
        ("GIT_CONFIG_COUNT", "1".to_owned()),
        ("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader".to_owned()),
        ("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: basic {credentials}")),
    ]
}

/// The `topcoat-asset` version whose format oxploy's bundler reads. A test checks it
/// against oxploy's own `Cargo.lock`.
const TOPCOAT_ASSET_VERSION: &str = "0.11.0";

/// Writes the files a Topcoat app declares with `asset!` to an `assets` folder next to
/// its binary, where the app loads them from, as `topcoat asset bundle` does. Returns
/// how many there are.
async fn bundle_assets(binary: PathBuf, cache: PathBuf) -> Result<usize, String> {
    let bundle = move || {
        let bytes = fs::read(&binary).map_err(|error| format!("the binary could not be read: {error}"))?;
        Bundler::new(&BundlerConfig::new().cache_dir(cache))
            .bundle(&bytes, binary.with_file_name("assets"))
            .map_err(|error| format!("the assets could not be bundled: {error}"))?;
        Ok(RawAsset::find_in_binary(&bytes).len())
    };
    tokio::task::spawn_blocking(bundle)
        .await
        .map_err(|error| error.to_string())?
}

/// Returns the version of `package` in the `Cargo.lock` cargo built `work_dir` with:
/// the nearest one at or above it, within the repository at `root`.
fn locked_version(work_dir: &Path, root: &Path, package: &str) -> Option<String> {
    let lockfile = work_dir
        .ancestors()
        .take_while(|dir| dir.starts_with(root))
        .map(|dir| dir.join("Cargo.lock"))
        .find(|path| path.is_file())?;
    version_in_lockfile(&fs::read_to_string(lockfile).ok()?, package)
}

/// Returns the version of `package` in the text of a `Cargo.lock`, whose packages each
/// start with a `name` line followed by a `version` line.
fn version_in_lockfile(lockfile: &str, package: &str) -> Option<String> {
    let name = format!("name = \"{package}\"");
    let mut lines = lockfile.lines();
    lines.find(|line| *line == name)?;
    let version = lines.next()?.strip_prefix("version = \"")?.strip_suffix('"')?;
    Some(version.to_owned())
}

/// Returns whether two versions are compatible by cargo's rules: the same major
/// version, or below 1.0 the same minor version, or below 0.1 the same patch.
fn compatible(a: &str, b: &str) -> bool {
    let fields = |version: &str| -> Vec<u64> {
        version
            .split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|field| field.parse().unwrap_or(u64::MAX))
            .collect()
    };
    let (a, b) = (fields(a), fields(b));
    // The first nonzero field decides compatibility, and the fields before it must
    // match.
    let decisive = a.iter().position(|field| *field != 0).unwrap_or(a.len().saturating_sub(1));
    a.len() > decisive && b.len() > decisive && a[..=decisive] == b[..=decisive]
}

/// Reads one line of cargo's JSON output: `None` if it is not JSON, `Some(None)` for a
/// message about something else, and `Some(Some((name, path)))` for a binary it built.
fn built_binary(line: &str) -> Option<Option<(String, PathBuf)>> {
    let message: serde_json::Value = serde_json::from_str(line).ok()?;
    let is_binary = message["reason"] == "compiler-artifact"
        && message["target"]["kind"]
            .as_array()
            .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"));
    let binary = is_binary
        .then(|| {
            let name = message["target"]["name"].as_str()?;
            let path = message["executable"].as_str()?;
            Some((name.to_owned(), PathBuf::from(path)))
        })
        .flatten();
    Some(binary)
}

/// Deletes the releases older than the live one and the [`KEPT_RELEASES`] before it.
fn prune(files: &AppFiles, live: u64) {
    let Ok(entries) = fs::read_dir(files.releases()) else {
        return;
    };
    let mut older: Vec<u64> = entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter(|id| *id < live)
        .collect();
    older.sort_unstable_by(|a, b| b.cmp(a));
    for id in older.into_iter().skip(KEPT_RELEASES) {
        let _ = fs::remove_dir_all(files.release(id));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_binaries_from_cargo_output() {
        let binary = r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"api"},"executable":"/t/release/api"}"#;
        let library = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"core"},"executable":null}"#;
        assert_eq!(
            built_binary(binary),
            Some(Some(("api".to_owned(), PathBuf::from("/t/release/api"))))
        );
        assert_eq!(built_binary(library), Some(None));
        assert_eq!(built_binary("   Compiling api v0.1.0"), None);
    }

    #[tokio::test]
    async fn bundles_a_topcoat_app_without_assets() {
        let dir = std::env::temp_dir().join(format!("oxploy-bundle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("app");
        // A binary without any `asset!`.
        fs::copy("/bin/sh", &binary).unwrap();

        let bundled = bundle_assets(binary, dir.join("cache")).await;

        let manifest = dir.join("assets").join("manifest.toml").is_file();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(bundled, Ok(0));
        assert!(manifest, "AssetBundle::load needs the manifest even when it lists nothing");
    }

    #[test]
    fn finds_versions_in_a_lockfile() {
        let lockfile = "version = 4\n\n[[package]]\nname = \"topcoat\"\nversion = \"0.11.0\"\n\n[[package]]\nname = \"topcoat-asset\"\nversion = \"0.11.2\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        assert_eq!(version_in_lockfile(lockfile, "topcoat-asset").as_deref(), Some("0.11.2"));
        assert_eq!(version_in_lockfile(lockfile, "axum"), None);
    }

    #[test]
    fn compares_versions_like_cargo() {
        assert!(compatible("0.11.4", "0.11.0"));
        assert!(!compatible("0.12.0", "0.11.0"));
        assert!(compatible("1.9.0", "1.2.3"));
        assert!(!compatible("2.0.0", "1.2.3"));
        assert!(!compatible("0.0.4", "0.0.3"));
        assert!(compatible("0.11.0-rc.1", "0.11.0"));
    }

    #[test]
    fn knows_its_own_asset_format() {
        let lockfile = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock")).unwrap();
        assert_eq!(
            version_in_lockfile(&lockfile, "topcoat-asset").as_deref(),
            Some(TOPCOAT_ASSET_VERSION),
            "update TOPCOAT_ASSET_VERSION with the topcoat-asset dependency"
        );
    }
}
