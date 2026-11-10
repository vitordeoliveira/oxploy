//! Runs each service's release as a process on this machine, listening on 127.0.0.1.
//!
//! A process outlives the panel: it runs in a process group of its own, so stopping the
//! panel does not stop it, and its PID is written next to its files, so a restarted
//! panel takes it back instead of starting a second copy. While the panel runs, it
//! starts a process again after it exits, waiting longer after each crash, and samples
//! how much of the machine each process takes.
//!
//! This is the native runtime. A Docker runtime would run containers behind the same
//! methods, published on the same port.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use sysinfo::{Pid, ProcessesToUpdate, System};
use toasty::Db;

use super::app_files::AppFiles;
use super::databases::{self, ManagedDatabase};
use super::deployment::Deployment;
use super::caddy::local_panel_address;
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
    pub binary: PathBuf,
    /// The folder the binary runs in: the build path in the release's code.
    pub work_dir: PathBuf,
    pub port: u64,
    pub variables: Vec<(String, String)>,
    /// The databases oxploy manages for the service, linked into the release at start.
    pub databases: Vec<ManagedDatabase>,
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
        let Some(binary) = &release.binary else {
            return Ok(None);
        };
        let files = AppFiles::of(&service.app_name);
        let source = files.source(release_id);
        let work_dir = match &release.build_path {
            Some(path) => source.join(path),
            None => source,
        };
        let databases = ManagedDatabase::of_service_in(db, service.id).await?;
        let oxploy = FromOxploy::of(db, service).await?;
        let variables = app_variables(service, &files, port, &databases, &oxploy);
        Ok(Some(Launch {
            service_id: service.id,
            binary: files.binary(release_id, binary),
            files,
            release_id,
            work_dir,
            port,
            variables,
            databases,
        }))
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
        if let Err(error) = databases::link_all(&launch.work_dir, &launch.files, &launch.databases) {
            note(&launch.files, &format!("The databases could not be linked: {error}"));
        }
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        writeln!(
            log,
            "==> {} Starting release #{} on 127.0.0.1:{}",
            time_zone::now().strftime("%Y-%m-%d %H:%M:%S"),
            launch.release_id,
            launch.port,
        )?;

        let child = Command::new(&launch.binary)
            .current_dir(&launch.work_dir)
            .env_clear()
            .envs(system_env())
            .envs(launch.variables.iter().map(|(key, value)| (key, value)))
            .env(PORT_VARIABLE, launch.port.to_string())
            .env(STORAGE_VARIABLE, &storage)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            // A group of its own keeps the app running when the panel stops, as with
            // Ctrl+C in the panel's terminal.
            .process_group(0)
            .spawn()?;
        fs::write(
            launch.files.pid_file(),
            format!("{}\n{}\n", child.id(), launch.release_id),
        )?;
        lock(&self.processes).insert(
            launch.service_id,
            Process {
                pid: child.id(),
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
            .or_else(|| running_from(files).map(|(pid, _, since)| Process {
                pid,
                child: None,
                started: Instant::now(),
                since,
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
        }
        let _ = fs::remove_file(files.pid_file());
    }

    /// Waits up to `timeout` for the service's new process to accept connections on its
    /// port.
    pub async fn wait_until_ready(&self, service_id: u64, port: u64, timeout: Duration) -> Readiness {
        let deadline = Instant::now() + timeout;
        loop {
            let exited = lock(&self.processes)
                .get_mut(&service_id)
                .map_or_else(|| Some("it is gone".to_owned()), Process::exited);
            if let Some(how) = exited {
                return Readiness::Exited(how);
            }
            let address = ("127.0.0.1", u16::try_from(port).unwrap_or(u16::MAX));
            if tokio::net::TcpStream::connect(address).await.is_ok() {
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
        let Some((pid, release_id, since)) = running_from(&launch.files) else {
            return false;
        };
        if release_id != launch.release_id {
            return false;
        }
        lock(&self.processes).insert(
            launch.service_id,
            Process {
                pid,
                child: None,
                started: Instant::now(),
                since,
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
        Ok(FromOxploy {
            mcp_url: format!("http://{}/mcp", local_panel_address()),
            tokens,
        })
    }
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
    // Only the app whose variables use one of them gets an organization's token.
    provided.extend(oxploy.tokens.iter().map(|(name, value)| (name.as_str(), value.clone())));
    let mut variables = set_by_databases.clone();
    variables.extend(environment::resolve(own, &provided));
    variables.push((MCP_URL_VARIABLE.to_owned(), oxploy.mcp_url.clone()));
    if let Some(token) = mcp_token {
        variables.push((MCP_TOKEN_VARIABLE.to_owned(), token.to_owned()));
    }
    variables
}

/// The variables of the panel's own environment that programs need, such as `PATH`.
/// The rest, such as the panel's `DATABASE_URL`, stay with the panel.
pub fn system_env() -> impl Iterator<Item = (&'static str, String)> {
    const PASSED: [&str; 8] = ["PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TZ", "TMPDIR"];
    PASSED
        .into_iter()
        .filter_map(|name| Some((name, std::env::var(name).ok()?)))
}

/// Returns the process in the app's PID file, the release it runs and when it started,
/// if it is alive and runs one of the app's binaries, which rules out a PID since reused
/// by another program.
fn running_from(files: &AppFiles) -> Option<(u32, u64, SystemTime)> {
    let text = fs::read_to_string(files.pid_file()).ok()?;
    let mut lines = text.lines();
    let pid: u32 = lines.next()?.parse().ok()?;
    let release_id: u64 = lines.next()?.parse().ok()?;
    let releases = fs::canonicalize(files.releases()).ok()?;
    let mut system = System::new().ok()?;
    let process_id = Pid::from_u32(pid);
    system.refresh_processes(ProcessesToUpdate::Some(&[process_id]), true);
    let process = system.process(process_id)?;
    let since = SystemTime::UNIX_EPOCH + Duration::from_secs(process.start_time());
    let exe = fs::canonicalize(process.exe()?).ok()?;
    exe.starts_with(&releases).then_some((pid, release_id, since))
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

    #[test]
    fn restarts_wait_longer_after_each_crash() {
        let waits: Vec<_> = (1..=9).map(|crashes| backoff(crashes).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }
}
