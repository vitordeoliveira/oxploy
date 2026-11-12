//! Runs each service's release as a process on this machine, listening on 127.0.0.1.
//!
//! A process outlives the panel: it runs in a process group of its own, so stopping the
//! panel does not stop it, and its PID is written next to its files, so a restarted
//! panel takes it back instead of starting a second copy. While the panel runs, it
//! starts a process again after it exits, waiting longer after each crash, and samples
//! how much of the machine each process takes.
//!
//! A service built with Docker runs the same way, through the `docker run` command that
//! runs its container: the command's exit is the container's, signals to the command
//! reach the container, and the container is published on 127.0.0.1 at the service's
//! port. See [`docker`](super::docker).

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use toasty::Db;

use super::app_files::AppFiles;
use super::databases::{self, ManagedDatabase};
use super::deployment::Deployment;
use super::caddy::local_panel_address;
use super::docker::{self, Container};
use super::environment::{self, MCP_TOKEN_VARIABLE, MCP_URL_VARIABLE, PORT_VARIABLE, STORAGE_VARIABLE};
use super::mcp::McpToken;
use super::project::Project;
use super::service::Service;
use super::time_zone;
use super::usage::Usage;

/// How often the panel checks that the apps that should run are running.
const WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// How long an app may take to exit after being asked to, before it is killed.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an app must run before its earlier crashes stop lengthening the wait
/// before the next restart.
const STABLE_AFTER: Duration = Duration::from_secs(60);

/// The longest wait before starting a crashed app again.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The largest an app's log grows before the next start moves it aside.
const MAX_LOG_LEN: u64 = 10 * 1024 * 1024;

/// What starting a service's release takes.
pub struct Launch {
    pub service_id: u64,
    pub files: AppFiles,
    pub release_id: u64,
    pub runs: Runs,
    /// The port on 127.0.0.1 the app answers at.
    pub port: u64,
    pub variables: Vec<(String, String)>,
    /// The databases oxploy manages for the service, linked into a native release at
    /// start.
    pub databases: Vec<ManagedDatabase>,
}

/// What a release runs.
pub enum Runs {
    /// A binary, in the folder it runs in: the build path in the release's code.
    Native { binary: PathBuf, work_dir: PathBuf },
    /// An image, in a container.
    Container(Container),
}

impl Launch {
    /// The folder a native release runs in.
    pub fn work_dir(&self) -> Option<&PathBuf> {
        match &self.runs {
            Runs::Native { work_dir, .. } => Some(work_dir),
            Runs::Container(_) => None,
        }
    }

    pub fn in_container(&self) -> bool {
        matches!(self.runs, Runs::Container(_))
    }
}

impl Launch {
    /// Returns how to start the service's release, or `None` if it has none.
    pub async fn of(db: &Db, service: &Service) -> toasty::Result<Option<Launch>> {
        let (Some(release_id), Some(port)) = (service.release_id, service.port) else {
            return Ok(None);
        };
        let Some(release) = Deployment::filter_by_id(release_id)
            .first()
            .exec(&mut db.clone())
            .await?
        else {
            return Ok(None);
        };
        let files = AppFiles::of(&service.app_name);
        let runs = match (&release.image, &release.binary) {
            (Some(image), _) => Runs::Container(container(service, image, port)),
            (None, Some(binary)) => {
                let source = files.source(release_id);
                let work_dir = match &release.build_path {
                    Some(path) => source.join(path),
                    None => source,
                };
                Runs::Native {
                    binary: files.binary(release_id, binary),
                    work_dir,
                }
            }
            (None, None) => return Ok(None),
        };
        let databases = ManagedDatabase::of_service_in(db, service.id).await?;
        let oxploy = FromOxploy::of(db, service).await?;
        // A release runs as it was built, even if the service builds another way since.
        let app_port = match &runs {
            Runs::Container(container) => container.inner_port,
            Runs::Native { .. } => port,
        };
        let variables = app_variables(service, &files, app_port, &databases, &oxploy);
        Ok(Some(Launch {
            service_id: service.id,
            files,
            release_id,
            runs,
            port,
            variables,
            databases,
        }))
    }
}

/// The container that runs `image` for the service, published at `port`.
pub fn container(service: &Service, image: &str, port: u64) -> Container {
    Container {
        image: image.to_owned(),
        name: docker::container_name(&service.app_name),
        network: docker::network(service.project_id),
        alias: service.app_name.clone(),
        port,
        inner_port: service.container_port.unwrap_or(port),
        storage: AppFiles::of(&service.app_name).storage(),
    }
}

/// What became of an app that was just started.
pub enum Readiness {
    /// It accepts connections on its port.
    Listening,
    /// It still runs but nothing answers on its port, as with a worker.
    NotListening,
    /// It exited, as described.
    Exited(String),
}

/// The processes of the services that run on this machine.
pub struct Runtime {
    db: Db,
    processes: Mutex<HashMap<u64, Process>>,
    /// Services a deployment or the user is starting or stopping, which the watcher
    /// leaves alone meanwhile.
    busy: Mutex<HashSet<u64>>,
    exits: Mutex<HashMap<u64, Exits>>,
    usage: Usage,
}

/// The times an app exited on its own since a deployment or the user last started it,
/// as this run of the panel saw them.
#[derive(Clone, Debug, Default)]
pub struct Exits {
    pub count: u32,
    /// When it last exited, and how.
    pub last: Option<(SystemTime, String)>,
}

/// A running app.
struct Process {
    pid: u32,
    /// The container the process runs, for an app that runs in Docker.
    container: Option<String>,
    /// The child, when this run of the panel started it. A process taken back after the
    /// panel restarted is not its child.
    child: Option<Child>,
    /// When this run of the panel started it, or took it back.
    started: Instant,
    /// When it started, as the system says for one taken back.
    since: SystemTime,
}

impl Process {
    /// Describes how the process exited, or returns `None` while it runs.
    fn exited(&mut self) -> Option<String> {
        match &mut self.child {
            Some(child) => match child.try_wait() {
                Ok(None) => None,
                Ok(Some(status)) => Some(status.to_string()),
                Err(error) => Some(error.to_string()),
            },
            None => (!is_alive(self.pid)).then(|| "it exited".to_owned()),
        }
    }
}

/// Keeps the watcher away from a service until dropped.
pub struct Hold<'a> {
    runtime: &'a Runtime,
    service_id: u64,
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        lock(&self.runtime.busy).remove(&self.service_id);
    }
}

/// The restarts of a service that keeps exiting.
#[derive(Default)]
struct Crashes {
    count: u32,
    retry_at: Option<Instant>,
}

impl Runtime {
    pub fn new(db: Db) -> Runtime {
        Runtime {
            db,
            processes: Mutex::default(),
            busy: Mutex::default(),
            exits: Mutex::default(),
            usage: Usage::new(),
        }
    }

    /// Starts checking, on a task of its own, that every service that should run does:
    /// taking back the ones a previous run of the panel started, and starting the rest.
    /// Also starts sampling their usage, on a thread of its own.
    pub fn watch(self: Arc<Self>) {
        let sampled = self.clone();
        thread::Builder::new()
            .name("app-usage".to_owned())
            .spawn(move || sampled.usage.sample_forever(|| sampled.running()))
            .expect("the app usage thread starts");
        tokio::spawn(async move {
            let mut crashes = HashMap::new();
            loop {
                if let Err(error) = self.check(&mut crashes).await {
                    eprintln!("oxploy: could not check the running apps: {error}");
                }
                tokio::time::sleep(WATCH_INTERVAL).await;
            }
        });
    }

    /// Keeps the watcher away from the service while the returned value lives.
    pub fn hold(&self, service_id: u64) -> Hold<'_> {
        lock(&self.busy).insert(service_id);
        Hold {
            runtime: self,
            service_id,
        }
    }

    /// When the service's process started, while it runs.
    pub fn running_since(&self, service_id: u64) -> Option<SystemTime> {
        let mut processes = lock(&self.processes);
        let process = processes.get_mut(&service_id)?;
        process.exited().is_none().then_some(process.since)
    }

    /// Returns whether the service's process runs.
    pub fn is_running(&self, service_id: u64) -> bool {
        lock(&self.processes)
            .get_mut(&service_id)
            .is_some_and(|process| process.exited().is_none())
    }

    /// The recent usage of each app.
    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    /// Returns how often the service's app exited on its own since it was last started.
    pub fn exits(&self, service_id: u64) -> Exits {
        lock(&self.exits)
            .get(&service_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Forgets the service's exits and usage, once it is deleted.
    pub fn forget(&self, service_id: u64) {
        lock(&self.exits).remove(&service_id);
        self.usage.forget(service_id);
    }

    /// Starts the release in `launch`, for a deployment or the user, which counts its
    /// exits again from none. Whatever ran for the service before must have been
    /// stopped.
    pub fn start(&self, launch: &Launch) -> io::Result<()> {
        lock(&self.exits).remove(&launch.service_id);
        self.spawn(launch)
    }

    /// The services whose process runs, with its PID.
    fn running(&self) -> Vec<(u64, u32)> {
        lock(&self.processes)
            .iter()
            .map(|(service_id, process)| (*service_id, process.pid))
            .collect()
    }

    /// Starts the release in `launch`.
    fn spawn(&self, launch: &Launch) -> io::Result<()> {
        let log_path = launch.files.app_log();
        if let Some(logs) = log_path.parent() {
            fs::create_dir_all(logs)?;
        }
        if fs::metadata(&log_path).is_ok_and(|metadata| metadata.len() > MAX_LOG_LEN) {
            let _ = fs::rename(&log_path, log_path.with_extension("log.1"));
        }
        let storage = launch.files.storage();
        fs::create_dir_all(&storage)?;
        // Databases added since the deploy get their links now.
        if let Runs::Native { work_dir, .. } = &launch.runs
            && let Err(error) = databases::link_all(work_dir, &launch.files, &launch.databases)
        {
            note(&launch.files, &format!("The databases could not be linked: {error}"));
        }
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let now = time_zone::now().strftime("%Y-%m-%d %H:%M:%S");
        let (release_id, port) = (launch.release_id, launch.port);
        match &launch.runs {
            Runs::Native { .. } => writeln!(log, "==> {now} Starting release #{release_id} on 127.0.0.1:{port}")?,
            Runs::Container(_) => writeln!(
                log,
                "==> {now} Starting release #{release_id} in a container, on 127.0.0.1:{port}"
            )?,
        }

        let mut command = match &launch.runs {
            Runs::Native { binary, work_dir } => {
                let mut command = Command::new(binary);
                command
                    .current_dir(work_dir)
                    .env_clear()
                    .envs(system_env())
                    .envs(launch.variables.iter().map(|(key, value)| (key, value)))
                    .env(PORT_VARIABLE, launch.port.to_string())
                    .env(STORAGE_VARIABLE, &storage);
                command
            }
            Runs::Container(container) => {
                // One left running, as after its docker command was killed, would hold
                // the name and the port.
                docker::remove_container(&container.name);
                docker::ensure_network(&container.network).map_err(io::Error::other)?;
                docker::Run::app(container, &launch.variables).command()
            }
        };
        let child = command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            // A group of its own keeps the app running when the panel stops, as with
            // Ctrl+C in the panel's terminal.
            .process_group(0)
            .spawn()?;
        let container = match &launch.runs {
            Runs::Native { .. } => None,
            Runs::Container(container) => Some(container.name.clone()),
        };
        fs::write(
            launch.files.pid_file(),
            format!(
                "{}\n{}\n{}",
                child.id(),
                launch.release_id,
                container.as_deref().map(|name| format!("{name}\n")).unwrap_or_default()
            ),
        )?;
        lock(&self.processes).insert(
            launch.service_id,
            Process {
                pid: child.id(),
                container,
                child: Some(child),
                started: Instant::now(),
                since: SystemTime::now(),
            },
        );
        Ok(())
    }

    /// Stops the service's process, if it runs: asks it and what it started in its group
    /// to exit, and kills them if they have not after [`STOP_TIMEOUT`].
    pub async fn stop(&self, service_id: u64, files: &AppFiles) {
        let process = lock(&self.processes)
            .remove(&service_id)
            .or_else(|| running_from(files).map(|running| Process {
                pid: running.pid,
                container: running.container,
                child: None,
                started: Instant::now(),
                since: running.since,
            }));
        if let Some(mut process) = process {
            signal_group(process.pid, libc::SIGTERM);
            let deadline = Instant::now() + STOP_TIMEOUT;
            // The app may exit before what it started, which goes on running in its group.
            while process.exited().is_none() || group_is_alive(process.pid) {
                if Instant::now() >= deadline {
                    signal_group(process.pid, libc::SIGKILL);
                    deadline_wait(&mut process).await;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            // Its docker command is gone, but a container it did not stop in time, or
            // whose command was killed, would go on running.
            if let Some(name) = process.container {
                let _ = tokio::task::spawn_blocking(move || docker::remove_container(&name)).await;
            }
        }
        let _ = fs::remove_file(files.pid_file());
    }

    /// Waits up to `timeout` for the service's new process to accept connections on its
    /// port. A container's port is Docker's, which accepts connections before the app
    /// in the container listens, so the app must also answer an HTTP request.
    pub async fn wait_until_ready(&self, launch: &Launch, timeout: Duration) -> Readiness {
        let (service_id, port) = (launch.service_id, launch.port);
        let deadline = Instant::now() + timeout;
        loop {
            let exited = lock(&self.processes)
                .get_mut(&service_id)
                .map_or_else(|| Some("it is gone".to_owned()), Process::exited);
            if let Some(how) = exited {
                return Readiness::Exited(how);
            }
            let address = ("127.0.0.1", u16::try_from(port).unwrap_or(u16::MAX));
            let ready = match launch.in_container() {
                false => tokio::net::TcpStream::connect(address).await.is_ok(),
                true => answers_http(address).await,
            };
            if ready {
                return Readiness::Listening;
            }
            if Instant::now() >= deadline {
                return Readiness::NotListening;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Makes sure each service that should run does, once.
    async fn check(&self, crashes: &mut HashMap<u64, Crashes>) -> toasty::Result<()> {
        let services = Service::all().exec(&mut self.db.clone()).await?;
        for service in services {
            let should_run = service.release_id.is_some() && service.stopped_at.is_none();
            if !should_run || lock(&self.busy).contains(&service.id) {
                crashes.remove(&service.id);
                continue;
            }

            let exited = {
                let mut processes = lock(&self.processes);
                match processes.get_mut(&service.id) {
                    None => None,
                    Some(process) => match process.exited() {
                        None => {
                            if process.started.elapsed() > STABLE_AFTER {
                                crashes.remove(&service.id);
                            }
                            continue;
                        }
                        Some(how) => {
                            // What the app started would go on running beside the next
                            // start, so it goes too, as in a container whose main process
                            // exits.
                            let pid = process.pid;
                            processes.remove(&service.id);
                            let left = group_is_alive(pid);
                            if left {
                                signal_group(pid, libc::SIGKILL);
                            }
                            Some((how, left))
                        }
                    },
                }
            };
            let Some(launch) = Launch::of(&self.db, &service).await? else {
                continue;
            };
            let crash = crashes.entry(service.id).or_default();
            if let Some((how, left)) = exited {
                crash.count += 1;
                let wait = backoff(crash.count);
                crash.retry_at = Some(Instant::now() + wait);
                if left {
                    note(&launch.files, "Stopped the processes the app left running.");
                }
                note(
                    &launch.files,
                    &format!("The app exited ({how}). Starting it again in {}s.", wait.as_secs()),
                );
                let mut exits = lock(&self.exits);
                let exits = exits.entry(service.id).or_default();
                exits.count += 1;
                exits.last = Some((SystemTime::now(), how));
            } else if crash.retry_at.is_none() && self.take_back(&launch) {
                continue;
            }
            if crash.retry_at.is_some_and(|retry_at| Instant::now() < retry_at) {
                continue;
            }
            if let Err(error) = self.spawn(&launch) {
                crash.count += 1;
                crash.retry_at = Some(Instant::now() + backoff(crash.count));
                note(&launch.files, &format!("The app could not start: {error}"));
            }
        }
        Ok(())
    }

    /// Tracks the process a previous run of the panel started for `launch`, if it still
    /// runs that release.
    fn take_back(&self, launch: &Launch) -> bool {
        let Some(running) = running_from(&launch.files) else {
            return false;
        };
        if running.release_id != launch.release_id {
            return false;
        }
        lock(&self.processes).insert(
            launch.service_id,
            Process {
                pid: running.pid,
                container: running.container,
                child: None,
                started: Instant::now(),
                since: running.since,
            },
        );
        true
    }
}

/// What oxploy tells an app about its organization's MCP: where this server's MCP
/// answers, and the organization's tokens available to its apps, by variable, which the
/// app's own variables can use.
#[derive(Clone, Debug, Default)]
pub struct FromOxploy {
    pub mcp_url: String,
    pub tokens: Vec<(String, String)>,
    /// The project's other apps, oldest first.
    pub apps: Vec<ProjectApp>,
}

/// Another app of the service's project, and where it answers.
#[derive(Clone, Debug)]
pub struct ProjectApp {
    /// The service's name.
    pub name: String,
    /// Its URL, host and port, each with its variable, as
    /// [`address_variables`](environment::address_variables) names them.
    pub variables: [(String, String); 3],
    /// Why a line cannot use them, if it cannot.
    pub blocked: Option<Blocked>,
}

/// Why an app cannot use where another app of its project answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// One of the names is a variable oxploy sets or an MCP token's, which wins.
    Clashes,
    /// The app runs in a container, and the other one natively, on 127.0.0.1, which a
    /// container does not reach.
    NativeFromContainer,
}

impl FromOxploy {
    /// What oxploy tells the service's app.
    pub async fn of(db: &Db, service: &Service) -> toasty::Result<FromOxploy> {
        let project = Project::filter_by_id(service.project_id)
            .first()
            .exec(&mut db.clone())
            .await?;
        let tokens = match project {
            Some(project) => McpToken::for_apps(db, project.organization_id).await?,
            None => Vec::new(),
        };
        let mut others = Service::filter_by_project_id(service.project_id)
            .exec(&mut db.clone())
            .await?;
        others.sort_by_key(|other| other.id);
        let apps: Vec<ProjectApp> = others
            .iter()
            .filter(|other| other.id != service.id)
            .filter_map(|other| {
                let (host, port, reached) = address(service, other)?;
                let [url_name, host_name, port_name] = environment::address_variables(&other.app_name);
                let clashes = [&url_name, &host_name, &port_name].iter().any(|name| {
                    environment::is_set_by_oxploy(name) || tokens.iter().any(|(token, _)| token == *name)
                });
                let blocked = match (clashes, reached) {
                    (true, _) => Some(Blocked::Clashes),
                    (false, false) => Some(Blocked::NativeFromContainer),
                    (false, true) => None,
                };
                Some(ProjectApp {
                    name: other.name.clone(),
                    variables: [
                        (url_name, format!("http://{host}:{port}")),
                        (host_name, host),
                        (port_name, port.to_string()),
                    ],
                    blocked,
                })
            })
            .collect();
        Ok(FromOxploy {
            mcp_url: format!("http://{}/mcp", local_panel_address()),
            tokens,
            apps,
        })
    }

    /// The variables starting with `OXPLOY_` that a line can use besides the ones oxploy
    /// sets for every app: the organization's tokens and the project's other apps'.
    pub fn usable(&self) -> Vec<&str> {
        let tokens = self.tokens.iter().map(|(name, _)| name.as_str());
        let apps = self
            .apps
            .iter()
            .filter(|app| app.blocked.is_none())
            .flat_map(|app| app.variables.iter().map(|(name, _)| name.as_str()));
        tokens.chain(apps).collect()
    }
}

/// Where `app` answers for `caller`, another app of its project, if it is an app with a
/// port, and whether `caller` reaches it there:
///
/// - a native app reaches any app at 127.0.0.1, where native apps listen and
///   containers are published;
/// - a container reaches another container of its project on their network, by its
///   app name, at the port its app listens on;
/// - a container does not reach a native app, whose 127.0.0.1 is not the container's.
fn address(caller: &Service, app: &Service) -> Option<(String, u64, bool)> {
    if app.is_external() {
        return None;
    }
    let port = app.port?;
    Some(match (caller.runs_in_docker(), app.runs_in_docker()) {
        (false, _) => ("127.0.0.1".to_owned(), port, true),
        (true, true) => (app.app_name.clone(), app.app_port().unwrap_or(port), true),
        (true, false) => ("127.0.0.1".to_owned(), port, false),
    })
}

/// The service's variables as its app and builds get them: those its databases set,
/// then its own, with each `${NAME}` filled in from what oxploy sets for the app, its
/// organization's MCP tokens in `oxploy`, or an earlier line, then where this server's
/// MCP answers and the token of the app's MCP server while it serves one.
pub fn app_variables(
    service: &Service,
    files: &AppFiles,
    port: u64,
    managed: &[ManagedDatabase],
    oxploy: &FromOxploy,
) -> Vec<(String, String)> {
    variables_with_port(service, files, &port.to_string(), managed, oxploy)
}

/// The variables of [`app_variables`], with `port` as text, so a service that has no
/// port yet can show `${PORT}` in its place.
pub fn variables_with_port(
    service: &Service,
    files: &AppFiles,
    port: &str,
    managed: &[ManagedDatabase],
    oxploy: &FromOxploy,
) -> Vec<(String, String)> {
    // Saved variables always parse: a problem is refused when saving.
    let own = environment::parse(
        service.environment.as_deref().unwrap_or(""),
        &databases::set_elsewhere(managed),
    )
    .unwrap_or_default();
    let set_by_databases = databases::variables(managed, files);
    let mcp_token = service.mcp_token_in_use();
    let mut provided = vec![
        (PORT_VARIABLE, port.to_owned()),
        (STORAGE_VARIABLE, files.storage().display().to_string()),
        (MCP_TOKEN_VARIABLE, mcp_token.unwrap_or_default().to_owned()),
        (MCP_URL_VARIABLE, oxploy.mcp_url.clone()),
    ];
    provided.extend(set_by_databases.iter().map(|(name, value)| (name.as_str(), value.clone())));
    // Only the app whose variables use one of them gets an organization's token, or
    // where another app of its project answers.
    provided.extend(oxploy.tokens.iter().map(|(name, value)| (name.as_str(), value.clone())));
    provided.extend(
        oxploy
            .apps
            .iter()
            .filter(|app| app.blocked.is_none())
            .flat_map(|app| app.variables.iter().map(|(name, value)| (name.as_str(), value.clone()))),
    );
    let mut variables = set_by_databases.clone();
    variables.extend(environment::resolve(own, &provided));
    variables.push((MCP_URL_VARIABLE.to_owned(), oxploy.mcp_url.clone()));
    if let Some(token) = mcp_token {
        variables.push((MCP_TOKEN_VARIABLE.to_owned(), token.to_owned()));
    }
    variables
}

/// The names of the variables of [`system_env`].
pub const SYSTEM_ENV: [&str; 8] = ["PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TZ", "TMPDIR"];

/// The variables of the panel's own environment that programs need, such as `PATH`.
/// The rest, such as the panel's `DATABASE_URL`, stay with the panel.
pub fn system_env() -> impl Iterator<Item = (&'static str, String)> {
    SYSTEM_ENV
        .into_iter()
        .filter_map(|name| Some((name, std::env::var(name).ok()?)))
}

/// An app's process, as its PID file tells a restarted panel.
struct Running {
    pid: u32,
    release_id: u64,
    since: SystemTime,
    container: Option<String>,
}

/// Returns the process in the app's PID file, the release it runs and when it started,
/// if it is alive and runs one of the app's binaries, or the docker command that runs
/// its container, which rules out a PID since reused by another program.
fn running_from(files: &AppFiles) -> Option<Running> {
    let text = fs::read_to_string(files.pid_file()).ok()?;
    let mut lines = text.lines();
    let pid: u32 = lines.next()?.parse().ok()?;
    let release_id: u64 = lines.next()?.parse().ok()?;
    let container = lines.next().filter(|name| !name.is_empty()).map(str::to_owned);
    let mut system = System::new().ok()?;
    let process_id = Pid::from_u32(pid);
    // Its command line names its container; a plain refresh reads only its binary.
    let read = ProcessRefreshKind::nothing()
        .with_exe(UpdateKind::Always)
        .with_cmd(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[process_id]), true, read);
    let process = system.process(process_id)?;
    let since = SystemTime::UNIX_EPOCH + Duration::from_secs(process.start_time());
    let ours = match &container {
        Some(name) => process.cmd().iter().any(|arg| arg.to_str() == Some(name)),
        None => {
            let releases = fs::canonicalize(files.releases()).ok()?;
            fs::canonicalize(process.exe()?).ok()?.starts_with(&releases)
        }
    };
    ours.then_some(Running {
        pid,
        release_id,
        since,
        container,
    })
}

/// Whether something answers an HTTP request at `address` within a moment.
async fn answers_http(address: (&str, u16)) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let attempt = async {
        let mut stream = tokio::net::TcpStream::connect(address).await.ok()?;
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .ok()?;
        let mut first = [0; 1];
        (stream.read(&mut first).await.ok()? > 0).then_some(())
    };
    tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Returns whether a process with this PID exists.
///
/// An app the panel started before an update ran the new version in its place is still
/// its child. Once such an app exits, it is reaped here, or it would stay a zombie,
/// which still exists.
fn is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let mut status = 0;
    // SAFETY: `WNOHANG` returns at once, and a process that is not a child of the panel
    // only gets an error back.
    if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid {
        return false;
    }
    // SAFETY: signal 0 sends nothing; it only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Returns whether a process still runs in the group the app with this PID led.
fn group_is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; it only checks that the group has a process. A
    // group's ID is not given to a new process while the group has one.
    unsafe { libc::kill(-pid, 0) == 0 }
}

/// Sends `signal` to the process group the app leads.
fn signal_group(pid: u32, signal: libc::c_int) {
    if let Ok(pid) = libc::pid_t::try_from(pid) {
        // SAFETY: a negative PID addresses the app's process group, which oxploy made
        // when it started the app.
        unsafe {
            libc::kill(-pid, signal);
        }
    }
}

/// Waits a moment for a killed process to be gone, so its port is free again.
async fn deadline_wait(process: &mut Process) {
    for _ in 0..20 {
        if process.exited().is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// How long to wait before the next start, after `crashes` crashes in a row: one
/// second, doubling up to [`MAX_BACKOFF`].
fn backoff(crashes: u32) -> Duration {
    let seconds = 1u64 << crashes.saturating_sub(1).min(6);
    Duration::from_secs(seconds).min(MAX_BACKOFF)
}

/// Adds a line from oxploy to the app's log, between the app's own lines.
pub(super) fn note(files: &AppFiles, text: &str) {
    let log = OpenOptions::new().create(true).append(true).open(files.app_log());
    if let Ok(mut log) = log {
        let now = time_zone::now().strftime("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(log, "==> {now} {text}");
    }
}

/// Locks a mutex, carrying on if a panic poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_container_is_ready_once_its_app_answers() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        // Docker's port accepts a connection and closes it while the app does not listen.
        let closing = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closing_port = closing.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = closing.accept().await {
                drop(stream);
            }
        });
        let answering = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let answering_port = answering.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = answering.accept().await {
                let _ = stream.write_all(b"HTTP/1.0 404 Not Found\r\n\r\n").await;
            }
        });
        assert!(!answers_http(("127.0.0.1", closing_port)).await);
        assert!(answers_http(("127.0.0.1", answering_port)).await);
    }

    #[test]
    fn restarts_wait_longer_after_each_crash() {
        let waits: Vec<_> = (1..=9).map(|crashes| backoff(crashes).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }
}
