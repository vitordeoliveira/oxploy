//! How much of the machine oxploy and the apps it runs take, sampled in the background
//! as often as the server's usage in [`monitoring`](super::monitoring) and kept as long,
//! in memory.
//!
//! An app's usage adds up its process and every process it started. oxploy's is its
//! own process, and apart from it the builds it runs: every process it started other
//! than the apps. Network traffic is not measured: processes share the machine's
//! network, which counts bytes per interface, not per process.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Add;
use std::sync::{RwLock, RwLockWriteGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use sysinfo::{
    CpuRefreshKind, Pid, Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System,
};
use tokio::sync::broadcast;

use super::monitoring::{HISTORY, INTERVAL};

/// What some processes took between two samples.
#[derive(Clone, Copy, Debug, Default)]
pub struct Load {
    /// The busy share of all CPUs, from 0 to 100.
    pub cpu: f32,
    /// The memory they hold.
    pub memory: u64,
    /// Bytes per second read from and written to the disks.
    pub disk_read: f64,
    pub disk_written: f64,
}

impl Add for Load {
    type Output = Load;

    fn add(self, other: Load) -> Load {
        Load {
            cpu: (self.cpu + other.cpu).min(100.0),
            memory: self.memory + other.memory,
            disk_read: self.disk_read + other.disk_read,
            disk_written: self.disk_written + other.disk_written,
        }
    }
}

/// One reading of an app's usage.
#[derive(Clone, Copy, Debug)]
pub struct UsageSample {
    pub at: SystemTime,
    /// The server's memory.
    pub memory_total: u64,
    pub load: Load,
    /// The app's processes, or `None` when it was not running.
    pub process: Option<AppProcess>,
}

/// The processes of a running app.
#[derive(Clone, Copy, Debug)]
pub struct AppProcess {
    pub pid: u32,
    pub started_at: SystemTime,
    /// Its own process and the ones it started.
    pub count: usize,
    /// Bytes read from and written to the disks since they started.
    pub disk_read: u64,
    pub disk_written: u64,
}

/// One reading of oxploy's own usage, with what the apps it runs took beside it.
#[derive(Clone, Copy, Debug)]
pub struct PanelSample {
    pub at: SystemTime,
    /// The server's memory.
    pub memory_total: u64,
    /// The panel's own process.
    pub panel: Load,
    /// The builds it runs, such as git and cargo.
    pub builds: Load,
    /// How many processes the builds run.
    pub build_processes: usize,
    /// Every app it runs, together.
    pub apps: Load,
}

/// The recent samples of oxploy and of every app, by service.
pub struct Usage {
    started_at: SystemTime,
    apps: RwLock<HashMap<u64, VecDeque<UsageSample>>>,
    panel: RwLock<VecDeque<PanelSample>>,
    sampled: broadcast::Sender<()>,
}

impl Usage {
    pub fn new() -> Usage {
        Usage {
            started_at: SystemTime::now(),
            apps: RwLock::default(),
            panel: RwLock::new(VecDeque::with_capacity(HISTORY)),
            sampled: broadcast::channel(1).0,
        }
    }

    /// When the panel started, give or take its first moments.
    pub fn started_at(&self) -> SystemTime {
        self.started_at
    }

    /// Returns the service's kept samples, oldest first. A service that stopped keeps
    /// them until ten minutes have passed.
    pub fn app(&self, service_id: u64) -> Vec<UsageSample> {
        self.apps
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&service_id)
            .map(|samples| samples.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Returns the latest sample of each app that runs, by service.
    pub fn running_apps(&self) -> Vec<(u64, UsageSample)> {
        self.apps
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter_map(|(service_id, samples)| {
                let latest = samples.back()?;
                latest.process.map(|_| (*service_id, *latest))
            })
            .collect()
    }

    /// Returns oxploy's kept samples, oldest first.
    pub fn panel(&self) -> Vec<PanelSample> {
        let samples = self
            .panel
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        samples.iter().copied().collect()
    }

    /// Returns a receiver that gets a message after each round of samples.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.sampled.subscribe()
    }

    /// Forgets the service's samples, once it is deleted.
    pub fn forget(&self, service_id: u64) {
        self.write_apps().remove(&service_id);
    }

    /// Samples, every [`INTERVAL`] and on the thread that calls it, oxploy and the apps
    /// `running` returns, as service IDs with the PID of their process. Returns only if
    /// this platform cannot report the usage of processes.
    pub fn sample_forever(&self, running: impl Fn() -> Vec<(u64, u32)>) {
        let mut system = match System::new() {
            Ok(system) => system,
            Err(error) => {
                eprintln!("oxploy: the usage of processes cannot be measured here: {error}");
                return;
            }
        };
        system.refresh_cpu_list(CpuRefreshKind::nothing());
        let cpus = system.cpus().len().max(1);
        system.refresh_memory();
        let memory_total = system.total_memory();
        let panel = Pid::from_u32(std::process::id());
        // CPU usage and disk rates are measured between two refreshes.
        refresh(&mut system);
        let mut refreshed = Instant::now();
        loop {
            thread::sleep(INTERVAL);
            refresh(&mut system);
            let seconds = refreshed.elapsed().as_secs_f64();
            refreshed = Instant::now();

            let at = SystemTime::now();
            let children = children(&system);
            let running = running();
            let apps: HashSet<Pid> = running.iter().map(|(_, pid)| Pid::from_u32(*pid)).collect();
            let sampled: HashMap<u64, UsageSample> = running
                .into_iter()
                .map(|(service_id, pid)| {
                    let pid = Pid::from_u32(pid);
                    let process = system.process(pid).filter(|process| !is_zombie(process));
                    let reading = Reading::of(&system, &children, [pid], &HashSet::new());
                    let sample = UsageSample {
                        at,
                        memory_total,
                        load: reading.load(cpus, seconds),
                        process: process.map(|process| AppProcess {
                            pid: pid.as_u32(),
                            started_at: started_at(process),
                            count: reading.count,
                            disk_read: reading.read_total,
                            disk_written: reading.written_total,
                        }),
                    };
                    (service_id, sample)
                })
                .collect();
            let mut own = Reading::default();
            if let Some(process) = system.process(panel) {
                own.add(process);
            }
            let builds = Reading::of(
                &system,
                &children,
                children.get(&panel).into_iter().flatten().copied(),
                &apps,
            );
            let apps = sampled
                .values()
                .map(|sample| sample.load)
                .fold(Load::default(), Load::add);
            self.record(at, memory_total, sampled);
            self.record_panel(PanelSample {
                at,
                memory_total,
                panel: own.load(cpus, seconds),
                builds: builds.load(cpus, seconds),
                build_processes: builds.count,
                apps,
            });
            // Nobody may be watching, which is fine.
            let _ = self.sampled.send(());
        }
    }

    /// Adds a round of the apps' samples. A service that stopped gets an idle sample
    /// instead, so its charts go on to show that, until its whole history is idle.
    fn record(&self, at: SystemTime, memory_total: u64, mut sampled: HashMap<u64, UsageSample>) {
        let mut apps = self.write_apps();
        for (service_id, history) in apps.iter_mut() {
            let sample = sampled.remove(service_id).unwrap_or(UsageSample {
                at,
                memory_total,
                load: Load::default(),
                process: None,
            });
            if history.len() == HISTORY {
                history.pop_front();
            }
            history.push_back(sample);
        }
        apps.retain(|_, history| history.iter().any(|sample| sample.process.is_some()));
        for (service_id, sample) in sampled {
            let mut history = VecDeque::with_capacity(HISTORY);
            history.push_back(sample);
            apps.insert(service_id, history);
        }
    }

    fn record_panel(&self, sample: PanelSample) {
        let mut samples = self
            .panel
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if samples.len() == HISTORY {
            samples.pop_front();
        }
        samples.push_back(sample);
    }

    fn write_apps(&self) -> RwLockWriteGuard<'_, HashMap<u64, VecDeque<UsageSample>>> {
        self.apps
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// What some processes took since the refresh before, and since they started.
#[derive(Clone, Copy, Debug, Default)]
struct Reading {
    /// A share of one CPU, in percent, as sysinfo reports it.
    cpu: f32,
    memory: u64,
    /// Bytes since the refresh before.
    read: u64,
    written: u64,
    /// Bytes since the processes started.
    read_total: u64,
    written_total: u64,
    count: usize,
}

impl Reading {
    /// Adds up the processes in `roots` and every process they started, leaving out the
    /// ones in `skip` with what those started.
    fn of(
        system: &System,
        children: &HashMap<Pid, Vec<Pid>>,
        roots: impl IntoIterator<Item = Pid>,
        skip: &HashSet<Pid>,
    ) -> Reading {
        let mut reading = Reading::default();
        // A PID reused while the processes were listed could make a loop.
        let mut seen = HashSet::new();
        let mut pending: Vec<Pid> = roots.into_iter().collect();
        while let Some(pid) = pending.pop() {
            if skip.contains(&pid) || !seen.insert(pid) {
                continue;
            }
            let Some(process) = system.process(pid) else {
                continue;
            };
            reading.add(process);
            pending.extend(children.get(&pid).into_iter().flatten());
        }
        reading
    }

    /// Adds one process. One that exited and waits to be reaped takes nothing.
    fn add(&mut self, process: &Process) {
        if is_zombie(process) {
            return;
        }
        let usage = process.disk_usage();
        self.cpu += process.cpu_usage();
        self.memory += process.memory();
        self.read += usage.read_bytes;
        self.written += usage.written_bytes;
        self.read_total += usage.total_read_bytes;
        self.written_total += usage.total_written_bytes;
        self.count += 1;
    }

    /// The load over the `seconds` since the refresh before, on a machine with `cpus`
    /// CPUs.
    fn load(&self, cpus: usize, seconds: f64) -> Load {
        let rate = |bytes: u64| if seconds > 0.0 { bytes as f64 / seconds } else { 0.0 };
        Load {
            cpu: (self.cpu / cpus as f32).min(100.0),
            memory: self.memory,
            disk_read: rate(self.read),
            disk_written: rate(self.written),
        }
    }
}

/// Refreshes what a sample reads of every process. Threads, which Linux lists as
/// processes, stay out: they share their process's memory, which would count again.
fn refresh(system: &mut System) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_disk_usage()
            .without_tasks(),
    );
}

/// The processes each process started.
fn children(system: &System) -> HashMap<Pid, Vec<Pid>> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    children
}

fn is_zombie(process: &Process) -> bool {
    process.status() == ProcessStatus::Zombie
}

fn started_at(process: &Process) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(process.start_time())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: SystemTime, running: bool) -> UsageSample {
        UsageSample {
            at,
            memory_total: 0,
            load: Load::default(),
            process: running.then_some(AppProcess {
                pid: 1,
                started_at: at,
                count: 1,
                disk_read: 0,
                disk_written: 0,
            }),
        }
    }

    #[test]
    fn stopped_apps_are_kept_until_their_history_is_idle() {
        let usage = Usage::new();
        let at = SystemTime::now();
        usage.record(at, 0, HashMap::from([(7, sample(at, true))]));
        for _ in 1..HISTORY {
            usage.record(at, 0, HashMap::new());
        }
        let samples = usage.app(7);
        assert_eq!(samples.len(), HISTORY);
        assert!(samples.first().unwrap().process.is_some());
        assert!(samples.last().unwrap().process.is_none());

        usage.record(at, 0, HashMap::new());
        assert!(usage.app(7).is_empty());
    }
}
