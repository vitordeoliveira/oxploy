//! The Monitoring tab: the CPU, memory and disk the app takes while it runs.
//!
//! Each new sample re-renders the readings over the page's WebSocket. The charts are
//! the server monitoring page's, and so is `monitoring.js`, which adds the readout under
//! the pointer.

use std::time::SystemTime;

use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use topcoat::{
    Result,
    asset::asset,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    runtime::connected,
    view::{View, attributes, component, emit, live, view},
};

use crate::app::dashboard::monitoring::{
    Line, Window, chart, fact, format_bytes, format_bytes_tick, format_duration, format_rate,
    format_rate_tick, legend, meter, metric, nice_bytes_ceiling, server_clock, share, thirds,
};
use crate::features::app_files::{AppFiles, size_of};
use crate::features::usage::UsageSample;
use crate::features::deploy::{AppStatus, Deployer};
use crate::features::monitoring::{INTERVAL, Monitor};
use crate::features::runtime::Exits;
use crate::features::service::Service;

/// The app's usage over the last ten minutes, refreshed after each sample.
#[component]
pub async fn monitoring_tab(cx: &Cx, service: &Service) -> Result<impl View> {
    let storage = size_of(&AppFiles::of(&service.app_name).storage());
    let cpus = Monitor::get(cx).host().cpu_count;

    Ok(view! {
        <div class="flex flex-col gap-4">
            (live! {
                let deployer = Deployer::get(cx);
                let mut sampled = deployer.usage().subscribe();
                loop {
                    let token = emit! {
                        readings(
                            samples: deployer.usage().app(service.id),
                            status: deployer.status(service),
                            exits: deployer.exits(service.id),
                            storage: storage,
                            cpus: cpus
                        )
                    }?;
                    if !connected(cx) {
                        break Ok(token);
                    }
                    // A lagging receiver only skipped samples; the next render shows the latest.
                    if let Err(RecvError::Closed) = sampled.recv().await {
                        break Ok(token);
                    }
                }
            })
            <p class="flex items-start gap-2 text-xs text-muted-foreground">
                icon(data: iconify_icon!("lucide:info"), size: 14, attrs: attributes! { class="mt-px shrink-0" })
                "Network traffic is not measured per app: an app that runs on the machine shares its network. Requests and traffic per app will come from Caddy, with domains."
            </p>
        </div>
        <script type="module" src=(asset!("assets/monitoring.js"))></script>
    })
}

/// The facts about the app's process, and the cards for every metric once there are
/// samples.
#[component]
async fn readings(
    samples: Vec<UsageSample>,
    status: AppStatus,
    exits: Exits,
    storage: u64,
    cpus: usize,
) -> Result<impl View> {
    Ok(view! {
        if status != AppStatus::NotDeployed {
            app_facts(latest: samples.last().copied(), exits: &exits, storage: storage)
        }
        if samples.is_empty() {
            <p class="rounded-xl border border-dashed border-border p-10 text-center text-sm text-muted-foreground">
                if status == AppStatus::Running {
                    "Collecting the first sample…"
                } else {
                    "The app is not running. Its usage shows here while it runs, and for ten minutes after it stops."
                }
            </p>
        } else {
            metrics(samples: &samples, cpus: cpus)
        }
    })
}

/// How long the app has run and as which processes, how often it crashed, and what its
/// storage folder takes.
#[component]
async fn app_facts(latest: Option<UsageSample>, exits: &Exits, storage: u64) -> Result<impl View> {
    let process = latest.and_then(|sample| sample.process);
    let crashes = match (exits.count, &exits.last) {
        (0, _) | (_, None) => "None since the last deploy or restart".to_owned(),
        (count, Some((at, how))) => format!(
            "{count} since the last deploy or restart · last at {}, {how}",
            server_clock(*at),
        ),
    };

    Ok(view! {
        <ul class="flex flex-wrap gap-2 text-xs">
            if let Some(process) = process {
                fact(
                    data: iconify_icon!("lucide:clock"),
                    label: "Up",
                    value: format_duration(
                        SystemTime::now()
                            .duration_since(process.started_at)
                            .unwrap_or_default()
                            .as_secs(),
                    )
                )
                fact(data: iconify_icon!("lucide:hash"), label: "PID", value: process.pid.to_string())
                fact(data: iconify_icon!("lucide:layers"), label: "Processes", value: process.count.to_string())
            }
            fact(data: iconify_icon!("lucide:refresh-ccw"), label: "Crashes", value: crashes)
            fact(data: iconify_icon!("lucide:folder"), label: "Storage", value: format_bytes(storage))
        </ul>
    })
}

/// The cards for every metric and the table of the latest samples.
#[component]
async fn metrics(samples: &[UsageSample], cpus: usize) -> Result<impl View> {
    let latest = *samples.last().expect("the caller checks for samples");
    let window = Window::new(samples.len());

    let cpu: Vec<f64> = samples.iter().map(|sample| f64::from(sample.load.cpu)).collect();
    let memory: Vec<f64> = samples.iter().map(|sample| sample.load.memory as f64).collect();
    let disk_rates = [
        samples.iter().map(|sample| sample.load.disk_read).collect::<Vec<_>>(),
        samples.iter().map(|sample| sample.load.disk_written).collect(),
    ];
    // An app takes a sliver of the server, so the axes fit what it took.
    let cpu_max = nice_percent_ceiling(cpu.iter().copied().fold(0.0, f64::max));
    let memory_max = nice_bytes_ceiling(memory.iter().copied().fold(0.0, f64::max));
    let disk_max = nice_bytes_ceiling(disk_rates.iter().flatten().copied().fold(0.0, f64::max));
    let since_start = match latest.process {
        Some(process) => format!(
            "Since it started: read {} · written {}",
            format_bytes(process.disk_read),
            format_bytes(process.disk_written),
        ),
        None => "Not running".to_owned(),
    };

    Ok(view! {
        <div class="flex flex-col gap-4" data-samples=(tooltip_data(samples))>
            <div class="grid gap-4 lg:grid-cols-2">
                metric(
                    data: iconify_icon!("lucide:cpu"),
                    title: "CPU",
                    value: format!("{:.1}%", latest.load.cpu),
                    detail: format!("Share of all {cpus} cores"),
                    meter(share: f64::from(latest.load.cpu) / 100.0, alert: "Busy", unit: "of the server")
                    chart(
                        window: &window,
                        fields: "cpu",
                        description: "The app's CPU usage over the last ten minutes",
                        max: cpu_max,
                        ticks: thirds(cpu_max, |percent| format!("{percent}%")),
                        lines: vec![Line { color: "text-primary", values: cpu }]
                    )
                )
                metric(
                    data: iconify_icon!("lucide:memory-stick"),
                    title: "Memory",
                    value: format_bytes(latest.load.memory),
                    detail: format!("of {} on the server", format_bytes(latest.memory_total)),
                    meter(share: share(latest.load.memory, latest.memory_total), alert: "Almost full", unit: "of the server")
                    chart(
                        window: &window,
                        fields: "memory",
                        description: "The app's memory over the last ten minutes",
                        max: memory_max,
                        ticks: thirds(memory_max, format_bytes_tick),
                        lines: vec![Line { color: "text-primary", values: memory }]
                    )
                )
                metric(
                    data: iconify_icon!("lucide:hard-drive-download"),
                    title: "Disk I/O",
                    value: format_rate(latest.load.disk_read + latest.load.disk_written),
                    detail: since_start,
                    attrs: attributes! { class="lg:col-span-2" },
                    legend(entries: vec![
                        ("text-chart-1", "Read", format_rate(latest.load.disk_read)),
                        ("text-chart-2", "Write", format_rate(latest.load.disk_written)),
                    ])
                    chart(
                        window: &window,
                        fields: "read,write",
                        description: "The app's disk read and write rates over the last ten minutes",
                        max: disk_max,
                        ticks: thirds(disk_max, format_rate_tick),
                        lines: vec![
                            Line { color: "text-chart-1", values: disk_rates[0].clone() },
                            Line { color: "text-chart-2", values: disk_rates[1].clone() },
                        ]
                    )
                )
            </div>
            latest_readings(samples: samples)
        </div>
    })
}

/// The latest samples as a table, so every value can be read without the charts.
#[component]
async fn latest_readings(samples: &[UsageSample]) -> Result<impl View> {
    let rows: Vec<UsageSample> = samples.iter().rev().take(5).copied().collect();

    Ok(view! {
        <section class="flex flex-col gap-2">
            <h2 class="text-sm font-medium">"Latest readings"</h2>
            <p class="text-xs text-muted-foreground">
                (format!("A new sample every {} seconds, kept for ten minutes.", INTERVAL.as_secs()))
            </p>
            <div class="overflow-x-auto rounded-xl border border-border">
                <table class="w-full text-left text-xs tabular-nums">
                    <thead class="bg-muted/60 text-muted-foreground">
                        <tr>
                            for heading in ["Time", "CPU", "Memory", "Disk read", "Disk write"] {
                                <th scope="col" class="px-3 py-2 font-medium whitespace-nowrap">(heading)</th>
                            }
                        </tr>
                    </thead>
                    <tbody>
                        for sample in rows {
                            <tr class="border-t border-border">
                                <td class="px-3 py-2 whitespace-nowrap">(server_clock(sample.at))</td>
                                if sample.process.is_some() {
                                    <td class="px-3 py-2 whitespace-nowrap">(format!("{:.1}%", sample.load.cpu))</td>
                                    <td class="px-3 py-2 whitespace-nowrap">(format_bytes(sample.load.memory))</td>
                                    <td class="px-3 py-2 whitespace-nowrap">(format_rate(sample.load.disk_read))</td>
                                    <td class="px-3 py-2 whitespace-nowrap">(format_rate(sample.load.disk_written))</td>
                                } else {
                                    <td colspan="4" class="px-3 py-2 whitespace-nowrap text-muted-foreground">"Not running"</td>
                                }
                            </tr>
                        }
                    </tbody>
                </table>
            </div>
        </section>
    })
}

/// Every sample's values, formatted, for the readout under the pointer.
fn tooltip_data(samples: &[UsageSample]) -> String {
    let rows: Vec<_> = samples
        .iter()
        .map(|sample| {
            let time = server_clock(sample.at);
            json!({
                "time": if sample.process.is_some() { time } else { format!("{time} · not running") },
                "cpu": [["CPU", format!("{:.1}%", sample.load.cpu)]],
                "memory": [["Memory", format_bytes(sample.load.memory)]],
                "read,write": [["Read", format_rate(sample.load.disk_read)], ["Write", format_rate(sample.load.disk_written)]],
            })
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}

/// The smallest of 1, 2, 5, 10, 20, 50 and 100 percent at or above `percent`.
fn nice_percent_ceiling(percent: f64) -> f64 {
    [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0]
        .into_iter()
        .find(|step| *step >= percent)
        .unwrap_or(100.0)
}
