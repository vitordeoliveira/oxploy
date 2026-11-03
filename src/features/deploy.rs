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
use super::deployment::{Deployment, DeploymentStatus};
use super::environment::{PORT_VARIABLE, STORAGE_VARIABLE};
use super::github::GithubApp;
use super::project::{Project, now};
use super::runtime::{Launch, Readiness, Runtime, app_variables, system_env};
use super::service::{BuildKind, Service};

/// How long a new release may take to start listening on its port.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// The ports oxploy gives apps, on 127.0.0.1.
const PORTS: std::ops::RangeInclusive<u64> = 10_000..=19_999;

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
    runtime: Arc<Runtime>,
    builds: Arc<Semaphore>,
    /// The services with a deployment in progress.
    deploying: Arc<Mutex<HashSet<u64>>>,
}

impl Deployer {
    /// Marks the deployments a previous run of the panel left unfinished as failed,
    /// then starts watching the apps that should run.
    pub async fn start(db: Db) -> toasty::Result<Deployer> {
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
        Ok(Deployer {
            db,
            runtime,
            builds: Arc::new(Semaphore::new(1)),
            deploying: Arc::default(),
        })
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

    pub fn is_deploying(&self, service_id: u64) -> bool {
        lock(&self.deploying).contains(&service_id)
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

    /// Stops the service and deletes its deployments and files, before the service
    /// itself is deleted.
    pub async fn forget(&self, service: &Service) -> toasty::Result<()> {
        let files = AppFiles::of(&service.app_name);
        let _hold = self.runtime.hold(service.id);
        self.runtime.stop(service.id, &files).await;
        Deployment::filter_by_service_id(service.id)
            .delete()
            .exec(&mut self.db.clone())
            .await?;
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

        let _build = match self.builds.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                log.step("Waiting for another build to finish");
                self.builds
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|error| error.to_string())?
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
        log.step(&format!("Fetching {repository} at {branch}"));
        let token = app
            .installation_token()
            .await
            .map_err(|error| error.to_string())?;
        let source = files.source(deployment.id);
        let _ = fs::remove_dir_all(&source);
        fs::create_dir_all(files.release(deployment.id)).map_err(|error| error.to_string())?;
        let mut clone = tokio::process::Command::new("git");
        clone
            .args(["clone", "--depth", "1", "--single-branch", "--no-tags", "--branch"])
            .arg(branch)
            .arg("--")
            .arg(format!("https://github.com/{repository}.git"))
            .arg(&source)
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
        let commit = git_output(&source, &["rev-parse", "HEAD"]).await;
        let message = git_output(&source, &["log", "-1", "--format=%s"]).await;
        if let Some(commit) = &commit {
            let short = commit.get(..7).unwrap_or(commit);
            log.line(&format!("Commit {short} {}", message.as_deref().unwrap_or("")));
        }
        deployment
            .update()
            .commit(commit)
            .commit_message(message)
            .build_path(service.build_path.clone())
            .exec(&mut db)
            .await
            .map_err(saved)?;

        // Build.
        let work_dir = match &service.build_path {
            Some(path) => source.join(path),
            None => source.clone(),
        };
        if !work_dir.is_dir() {
            let path = service.build_path.as_deref().unwrap_or("");
            return Err(format!("there is no folder /{path} in the repository"));
        }
        // The port comes first, so the variables can use `${PORT}` in builds too.
        let port = match service.port {
            Some(port) => port,
            None => self.assign_port(service).await?,
        };
        let variables = app_variables(service, files, port);
        let build = Build {
            service,
            files,
            deployment_id: deployment.id,
            source: &source,
            work_dir: &work_dir,
            variables: &variables,
        };
        let name = match service.builds_with() {
            BuildKind::Cargo => build_with_cargo(&build, log).await?,
            BuildKind::Script => build_with_script(&build, log).await?,
        };
        let binary = files.binary(deployment.id, &name);
        deployment
            .update()
            .binary(Some(name))
            .exec(&mut db)
            .await
            .map_err(saved)?;
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
        prune(files, deployment.id);
        Ok(())
    }

    /// The service's GitHub App, if it still exists in the service's organization.
    async fn github_app(&self, service: &Service, app_id: u64) -> toasty::Result<Option<GithubApp>> {
        let mut db = self.db.clone();
        let Some(project) = Project::filter_by_id(service.project_id).first().exec(&mut db).await?
        else {
            return Ok(None);
        };
        let app = GithubApp::filter_by_id(app_id).first().exec(&mut db).await?;
        Ok(app.filter(|app| app.organization_id == project.organization_id))
    }

    /// Gives the service a port on 127.0.0.1 that no other service has and nothing
    /// listens on.
    async fn assign_port(&self, service: &Service) -> Result<u64, String> {
        let mut db = self.db.clone();
        let services = Service::all()
            .exec(&mut db)
            .await
            .map_err(|error| error.to_string())?;
        let taken: HashSet<_> = services.iter().filter_map(|service| service.port).collect();
        let port = PORTS
            .into_iter()
            .find(|port| {
                !taken.contains(port)
                    && u16::try_from(*port)
                        .is_ok_and(|port| std::net::TcpListener::bind(("127.0.0.1", port)).is_ok())
            })
            .ok_or("there is no free port left for apps")?;
        Service::filter_by_id(service.id)
            .update()
            .port(Some(port))
            .exec(&mut db)
            .await
            .map_err(|error| error.to_string())?;
        Ok(port)
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

/// Builds with `cargo build --release`, keeps the binary in the release, and bundles
/// the assets of a Topcoat app. Returns the binary's name.
async fn build_with_cargo(build: &Build<'_>, log: &mut DeployLog) -> Result<String, String> {
    let service = build.service;
    if !build.work_dir.join("Cargo.toml").is_file() {
        let path = service.build_path.as_deref().unwrap_or("");
        return Err(format!("there is no Cargo.toml in /{path}"));
    }
    let toolchain = match &service.rust_version {
        Some(version) => format!(" (Rust {version})"),
        None => String::new(),
    };
    log.step(&format!("Building with {}{toolchain}", service.build_command()));
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
