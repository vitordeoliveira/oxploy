//! The server's resource usage, sampled in the background.
//!
//! The numbers describe the whole machine, so every organization sees the same ones.
//! A thread samples them every [`INTERVAL`] and keeps the last [`HISTORY`] samples in
//! memory; restarting the panel starts the history over.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, SystemTime};

use sysinfo::{Disks, Networks, System};
use tokio::sync::broadcast;
use topcoat::context::{Cx, app_context};

/// How often the server samples its usage.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// How many samples are kept: ten minutes at [`INTERVAL`].
pub const HISTORY: usize = 300;

/// One reading of the server's usage.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub at: SystemTime,
    /// The busy share of all CPUs, from 0 to 100.
    pub cpu: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    /// Space on the disk mounted at `/`.
    pub disk_used: u64,
    pub disk_total: u64,
    /// Bytes read from and written to the disks since the server booted.
    pub disk_read: u64,
    pub disk_written: u64,
    /// Bytes received and sent over the network since the server booted.
    pub network_received: u64,
    pub network_sent: u64,
}

/// Bytes per second moved between two samples.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rates {
    pub disk_read: f64,
    pub disk_written: f64,
    pub network_received: f64,
    pub network_sent: f64,
}

impl Sample {
    /// Returns the rates from `previous` to this sample. A counter that went backwards,
    /// as when a network interface restarts, counts as zero.
    pub fn rates_since(&self, previous: &Sample) -> Rates {
        let seconds = self
            .at
            .duration_since(previous.at)
            .unwrap_or_default()
            .as_secs_f64();
        if seconds == 0.0 {
            return Rates::default();
        }
        let rate = |now: u64, before: u64| now.saturating_sub(before) as f64 / seconds;
        Rates {
            disk_read: rate(self.disk_read, previous.disk_read),
            disk_written: rate(self.disk_written, previous.disk_written),
            network_received: rate(self.network_received, previous.network_received),
            network_sent: rate(self.network_sent, previous.network_sent),
        }
    }
}

/// Facts about the server that do not change while the panel runs.
#[derive(Clone, Debug)]
pub struct Host {
    pub name: String,
    pub os: String,
    pub cpu_brand: String,
    pub cpu_count: usize,
    pub booted_at: SystemTime,
}

/// The server's recent samples, shared by every request.
pub struct Monitor {
    host: Host,
    samples: Arc<RwLock<VecDeque<Sample>>>,
    sampled: broadcast::Sender<()>,
}

impl Monitor {
    /// Starts sampling on a thread of its own. Fails when this platform cannot report
    /// its usage.
    pub fn start() -> Result<Monitor, sysinfo::Error> {
        let mut system = System::new()?;
        let disks = Disks::new_with_refreshed_list()?;
        let networks = Networks::new_with_refreshed_list()?;
        system.refresh_cpu_all();
        let uptime = System::uptime().unwrap_or(0);
        let host = Host {
            name: System::host_name().unwrap_or_else(|_| "unknown".to_owned()),
            os: System::long_os_version().unwrap_or_else(|_| "unknown".to_owned()),
            cpu_brand: system
                .cpus()
                .first()
                .map(|cpu| cpu.brand().trim().to_owned())
                .unwrap_or_default(),
            cpu_count: system.cpus().len(),
            booted_at: SystemTime::now() - Duration::from_secs(uptime),
        };

        let samples = Arc::new(RwLock::new(VecDeque::with_capacity(HISTORY)));
        let (sampled, _) = broadcast::channel(1);
        let monitor = Monitor {
            host,
            samples: samples.clone(),
            sampled: sampled.clone(),
        };
        thread::Builder::new()
            .name("monitor".to_owned())
            .spawn(move || sample_forever(system, disks, networks, &samples, &sampled))
            .expect("the monitor thread starts");
        Ok(monitor)
    }

    /// Returns the monitor registered on the router.
    pub fn get(cx: &Cx) -> &Monitor {
        app_context::<Monitor>(cx)
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Returns the kept samples, oldest first.
    pub fn samples(&self) -> Vec<Sample> {
        let samples = self
            .samples
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        samples.iter().copied().collect()
    }

    /// Returns a receiver that gets a message after each new sample.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.sampled.subscribe()
    }
}

fn sample_forever(
    mut system: System,
    mut disks: Disks,
    mut networks: Networks,
    samples: &RwLock<VecDeque<Sample>>,
    sampled: &broadcast::Sender<()>,
) {
    loop {
        // CPU usage is measured between two refreshes, so the first sample waits a
        // full interval.
        thread::sleep(INTERVAL);
        system.refresh_cpu_usage();
        system.refresh_memory();
        disks.refresh(true);
        networks.refresh(true);

        let sample = read(&system, &disks, &networks);
        {
            let mut samples = samples
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if samples.len() == HISTORY {
                samples.pop_front();
            }
            samples.push_back(sample);
        }
        // Nobody may be watching, which is fine.
        let _ = sampled.send(());
    }
}

fn read(system: &System, disks: &Disks, networks: &Networks) -> Sample {
    // The disk holding `/`, or the largest one when nothing is mounted there.
    let root = disks
        .iter()
        .find(|disk| disk.mount_point() == std::path::Path::new("/"))
        .or_else(|| disks.iter().max_by_key(|disk| disk.total_space()));
    let (disk_used, disk_total) = root.map_or((0, 0), |disk| {
        let total = disk.total_space();
        (total.saturating_sub(disk.available_space()), total)
    });

    // A device mounted at several paths is counted once.
    let mut devices = HashSet::new();
    let (mut disk_read, mut disk_written) = (0, 0);
    for disk in disks
        .iter()
        .filter(|disk| devices.insert(disk.name().to_owned()))
    {
        let usage = disk.usage();
        disk_read += usage.total_read_bytes;
        disk_written += usage.total_written_bytes;
    }

    let (mut network_received, mut network_sent) = (0, 0);
    for (name, data) in networks {
        if counts_as_server_traffic(&name.to_string_lossy()) {
            network_received += data.total_received();
            network_sent += data.total_transmitted();
        }
    }

    Sample {
        at: SystemTime::now(),
        cpu: system.global_cpu_usage(),
        memory_used: system.used_memory(),
        memory_total: system.total_memory(),
        disk_used,
        disk_total,
        disk_read,
        disk_written,
        network_received,
        network_sent,
    }
}

/// Skips loopback and the virtual interfaces of containers and bridges, whose traffic
/// also crosses a real interface and would otherwise count twice.
fn counts_as_server_traffic(interface: &str) -> bool {
    const VIRTUAL: [&str; 7] = ["lo", "veth", "docker", "br-", "virbr", "cni", "flannel"];
    !VIRTUAL.iter().any(|prefix| interface.starts_with(prefix))
}
