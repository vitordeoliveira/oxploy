//! The `server_status` tool: the machine, oxploy itself, and how many apps stand where.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::{duration, mib, percent, round, seconds_since, status_code, system_moment};
use crate::db::db;
use crate::features::caddy::{Caddy, CaddyStatus};
use crate::features::deploy::Deployer;
use crate::features::monitoring::Monitor;
use crate::features::service::Service;
use crate::features::update::{Step, Updater, VERSION};
use crate::features::usage::Load;

/// The name agents call the tool by.
pub const NAME: &str = "server_status";

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "How the server is doing now. The machine: its name, \
    system, CPUs and uptime, and its CPU, memory, disk and network now, with the peaks \
    of the last 10 minutes. oxploy itself: its version, whether an update is out and \
    how far the one being installed got, \
    whether Caddy, the web server in front of the apps, runs oxploy's configuration, \
    the deployments in progress, and what the panel, its builds and all the apps \
    together take. And how many apps are running, stopped, restarting after a crash, \
    deploying or never deployed, and how many are external MCP servers that run \
    elsewhere. CPU is the busy share of all CPUs together, from 0 to \
    100.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "Server status",
        "description": DESCRIPTION,
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "openWorldHint": false },
    })
}

/// The tool takes no arguments, and refuses any.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {}

/// Reads the server's state. The inner error says what is wrong with the arguments;
/// the outer one is a failure of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    if let Err(refusal) = super::arguments::<Arguments>(arguments) {
        return Ok(Err(refusal));
    }
    let deployer = Deployer::get(cx);
    let services = Service::all().exec(&mut db(cx)).await?;
    let mut apps = json!({
        "total": services.len(),
        "running": 0,
        "stopped": 0,
        "restarting": 0,
        "deploying": 0,
        "not_deployed": 0,
        "external": 0,
    });
    for service in &services {
        let code = if service.is_external() {
            "external"
        } else {
            status_code(deployer.status(service))
        };
        apps[code] = json!(apps[code].as_u64().unwrap_or(0) + 1);
    }

    Ok(Ok(json!({
        "machine": machine(cx),
        "oxploy": oxploy(cx),
        "apps": apps,
    })))
}

/// The machine, its readings now, and its peaks in the history kept in memory.
fn machine(cx: &Cx) -> Value {
    let monitor = Monitor::get(cx);
    let host = monitor.host();
    let samples = monitor.samples();
    let now = samples.last().map(|latest| {
        let rates = samples
            .len()
            .checked_sub(2)
            .map(|previous| latest.rates_since(&samples[previous]));
        json!({
            "cpu_percent": round(f64::from(latest.cpu)),
            "memory": {
                "used_mib": mib(latest.memory_used as f64),
                "total_mib": mib(latest.memory_total as f64),
                "percent": percent(latest.memory_used, latest.memory_total),
            },
            "disk": {
                "used_gib": round(latest.disk_used as f64 / GIB),
                "total_gib": round(latest.disk_total as f64 / GIB),
                "percent": percent(latest.disk_used, latest.disk_total),
            },
            "disk_read_kib_per_second": rates.map(|rates| round(rates.disk_read / KIB)),
            "disk_written_kib_per_second": rates.map(|rates| round(rates.disk_written / KIB)),
            "network_received_kib_per_second": rates.map(|rates| round(rates.network_received / KIB)),
            "network_sent_kib_per_second": rates.map(|rates| round(rates.network_sent / KIB)),
        })
    });
    let cpu_peak = samples.iter().map(|sample| sample.cpu).fold(0.0, f32::max);
    let memory_peak = samples.iter().map(|sample| sample.memory_used).max().unwrap_or(0);

    json!({
        "name": host.name,
        "system": host.os,
        "cpu": host.cpu_brand,
        "cpus": host.cpu_count,
        "booted_at": system_moment(host.booted_at),
        "uptime": duration(seconds_since(host.booted_at)),
        "now": now,
        "last_10_minutes": {
            "cpu_percent_peak": round(f64::from(cpu_peak)),
            "memory_used_mib_peak": mib(memory_peak as f64),
        },
    })
}

/// oxploy itself: its version, updates, Caddy, deployments and what it takes.
fn oxploy(cx: &Cx) -> Value {
    let deployer = Deployer::get(cx);
    let updater = Updater::get(cx);
    let started_at = deployer.usage().started_at();
    let check = updater.check();
    let caddy = match Caddy::get(cx).status() {
        CaddyStatus::Unknown => "not checked yet".to_owned(),
        CaddyStatus::Running => "running oxploy's configuration".to_owned(),
        CaddyStatus::Unreachable(error) => format!("unreachable: {error}"),
        CaddyStatus::Refused(error) => format!("refused oxploy's configuration: {error}"),
    };
    let now = deployer.usage().panel().last().map(|sample| {
        json!({
            "panel": load(sample.panel),
            "builds": { "processes": sample.build_processes, "load": load(sample.builds) },
            "apps": load(sample.apps),
        })
    });

    json!({
        "version": VERSION,
        "started_at": system_moment(started_at),
        "uptime": duration(seconds_since(started_at)),
        "update_available": updater.available().map(|release| release.version),
        "update_checked_at": check.at.and_then(super::moment),
        "update_installing": updater.installation().map(|installation| json!({
            "version": installation.version,
            "step": step_code(&installation.step),
        })),
        "caddy": caddy,
        "deployments_in_progress": deployer.deploying(),
        "now": now,
    })
}

/// How far the installation of an update got, as the tools say it, with why it failed
/// when it did.
pub(super) fn step_code(step: &Step) -> String {
    match step {
        Step::Downloading => "downloading".to_owned(),
        Step::Checking => "checking".to_owned(),
        Step::WaitingForDeployments(count) => format!("waiting for {count} deployments"),
        Step::Restarting => "restarting".to_owned(),
        Step::Failed(why) => format!("failed: {why}"),
    }
}

/// What some processes take.
fn load(load: Load) -> Value {
    json!({ "cpu_percent": round(f64::from(load.cpu)), "memory_mib": mib(load.memory as f64) })
}

const KIB: f64 = 1024.0;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
