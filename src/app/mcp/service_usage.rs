//! The `service_usage` tool: what one app took over the last 10 minutes.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::{crashes, mib, round, service_named, status_code, system_moment};
use crate::features::app_files::{AppFiles, size_of};
use crate::features::deploy::Deployer;

/// The name agents call the tool by.
pub const NAME: &str = "service_usage";

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "What one app takes: its status; its process ID, when it \
    started and how many processes it runs; its CPU, memory and disk reads and writes \
    now; the average and peak of its CPU and memory over the last 10 minutes it ran, \
    sampled every 2 seconds; how often it exited on its own since it was last deployed \
    or started; and the size of its storage folder, where its data and databases live. \
    CPU is the busy share of all CPUs together, from 0 to 100.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "An app's usage",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "app_name": {
                    "type": "string",
                    "description": "The app, by its app_name, as list_services returns it.",
                },
            },
            "required": ["app_name"],
            "additionalProperties": false,
        },
        "annotations": { "readOnlyHint": true, "openWorldHint": false },
    })
}

/// The tool's arguments, as the agent sends them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    app_name: String,
}

/// Reads the app's usage. The inner error says what is wrong with the arguments; the
/// outer one is a failure of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    let arguments: Arguments = match super::arguments(arguments) {
        Ok(arguments) => arguments,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let service = match service_named(cx, &arguments.app_name).await? {
        Ok(service) => service,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let deployer = Deployer::get(cx);
    let samples = deployer.usage().app(service.id);
    // Only the samples taken while it ran: the others read zero.
    let running: Vec<_> = samples.iter().filter(|sample| sample.process.is_some()).collect();
    let latest = samples.last().and_then(|sample| Some((sample.process?, sample.load)));
    let last_10_minutes = (!running.is_empty()).then(|| {
        let count = running.len() as f64;
        let cpu: Vec<f64> = running.iter().map(|sample| f64::from(sample.load.cpu)).collect();
        let memory: Vec<f64> = running.iter().map(|sample| sample.load.memory as f64).collect();
        json!({
            "samples": running.len(),
            "cpu_percent_average": round(cpu.iter().sum::<f64>() / count),
            "cpu_percent_peak": round(cpu.iter().copied().fold(0.0, f64::max)),
            "memory_mib_average": mib(memory.iter().sum::<f64>() / count),
            "memory_mib_peak": mib(memory.iter().copied().fold(0.0, f64::max)),
        })
    });
    let storage = size_of(&AppFiles::of(&service.app_name).storage());

    Ok(Ok(json!({
        "app_name": service.app_name,
        "status": status_code(deployer.status(&service)),
        "process": latest.map(|(process, _)| json!({
            "pid": process.pid,
            "started_at": system_moment(process.started_at),
            "processes": process.count,
        })),
        "now": latest.map(|(_, load)| json!({
            "cpu_percent": round(f64::from(load.cpu)),
            "memory_mib": mib(load.memory as f64),
            "disk_read_kib_per_second": round(load.disk_read / 1024.0),
            "disk_written_kib_per_second": round(load.disk_written / 1024.0),
        })),
        "last_10_minutes": last_10_minutes,
        "crashes": crashes(deployer, service.id),
        "storage_mib": mib(storage as f64),
    })))
}
