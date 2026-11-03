//! What oxploy itself takes of the server: the panel's process, the builds it runs for
//! deployments, and the files it keeps. The charts can also show the apps it runs
//! beside it, all together.

use std::fs;
use std::time::SystemTime;

use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use topcoat::{
    Result,
    asset::asset,
    context::Cx,
    icon::iconify::iconify_icon,
    router::{href, page, query_params},
    runtime::connected,
    view::{View, attributes, class, component, emit, live, view},
};

use super::{
    Line, Section, Window, chart, fact, format_bytes, format_bytes_tick, format_duration,
    format_rate, format_rate_tick, legend, metric, nice_bytes_ceiling, nice_percent_ceiling,
    page_heading, server_clock, share, thirds,
};
use crate::app::dashboard::require_server_owner;
use crate::db;
use crate::features::app_files::{Footprint, data_dir};
use crate::features::deploy::Deployer;
use crate::features::monitoring::{INTERVAL, Monitor};
use crate::features::project::Project;
use crate::features::service::Service;
use crate::features::usage::{Load, PanelSample, Usage};

/// Whether the charts also show the apps: `?with=apps`.
#[query_params(error = bad_request)]
struct Shown {
    with: Option<String>,
}

#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    require_server_owner(cx).await?;
    let with_apps = query_params::<Shown>(cx)?.with.as_deref() == Some("apps");
    let up = SystemTime::now()
        .duration_since(Deployer::get(cx).usage().started_at())
        .unwrap_or_default()
        .as_secs();
    let cpus = Monitor::get(cx).host().cpu_count;
    // Measuring reads through every build cache, which can take a while.
    let footprint = tokio::task::spawn_blocking(Footprint::measure)
        .await
        .unwrap_or_default();
    let database = Database::measure();

    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            page_heading(
                current: Section::Oxploy,
                text: "What oxploy itself takes of the machine: the panel, the builds it runs for deployments, and the files it keeps. Each app's own usage is in its service's Monitoring tab.",
                <div class="flex flex-wrap items-center justify-between gap-3">
                    <ul class="flex flex-wrap gap-2 text-xs">
                        fact(data: iconify_icon!("lucide:tag"), label: "Version", value: env!("CARGO_PKG_VERSION").to_owned())
                        fact(data: iconify_icon!("lucide:clock"), label: "Up", value: format_duration(up))
                        fact(data: iconify_icon!("lucide:hash"), label: "PID", value: std::process::id().to_string())
                    </ul>
                    apps_toggle(with_apps: with_apps)
                </div>
            )
            (live! {
                let usage = Deployer::get(cx).usage();
                let mut sampled = usage.subscribe();
                loop {
                    let apps = if with_apps {
                        running_apps(cx, usage).await?
                    } else {
                        Vec::new()
                    };
                    let token = emit! {
                        readings(
                            samples: usage.panel(),
                            sources: sources(with_apps),
                            apps: apps,
                            cpus: cpus,
                            footprint: footprint,
                            database: &database
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
        </div>
        <script type="module" src=(asset!("assets/monitoring.js"))></script>
    })
}

/// The choice between charts of oxploy alone, scaled to it, and charts with the apps
/// it runs beside it, which usually take far more.
#[component]
async fn apps_toggle(cx: &Cx, with_apps: bool) -> Result<impl View> {
    let alone = href!(page).resolve(cx);
    let options = [
        (false, "oxploy only", alone.clone()),
        (true, "With the apps", format!("{alone}?with=apps")),
    ];

    Ok(view! {
        <nav aria-label="What the charts show" class="inline-flex shrink-0 rounded-lg border border-border bg-muted/60 p-0.5 text-sm">
            for (apps, label, url) in options {
                <a
                    href=(url)
                    aria-current=((apps == with_apps).then_some("page"))
                    class=(class!(
                        "rounded-md px-3 py-1 font-medium transition-colors",
                        if apps == with_apps {
                            "bg-background text-foreground shadow-xs ring-1 ring-border"
                        } else {
                            "text-muted-foreground hover:text-foreground"
                        },
                    ))
                >
                    (label)
                </a>
            }
        </nav>
    })
}

/// What a line of the CPU and memory charts adds up.
#[derive(Clone, Copy)]
struct Source {
    name: &'static str,
    color: &'static str,
    load: fn(&PanelSample) -> Load,
}

/// The panel and its builds, then the apps when they are shown.
fn sources(with_apps: bool) -> Vec<Source> {
    let mut sources = vec![
        Source { name: "Panel", color: "text-chart-1", load: |sample| sample.panel },
        Source { name: "Builds", color: "text-chart-2", load: |sample| sample.builds },
    ];
    if with_apps {
        sources.push(Source { name: "Apps", color: "text-chart-3", load: |sample| sample.apps });
    }
    sources
}

/// An app that runs now, with what it takes.
struct RunningApp {
    name: String,
    project: String,
    load: Load,
}

/// The apps that run now, the one with the most memory first.
async fn running_apps(cx: &Cx, usage: &Usage) -> Result<Vec<RunningApp>> {
    let running = usage.running_apps();
    if running.is_empty() {
        return Ok(Vec::new());
    }
    let services = Service::all().exec(&mut db::db(cx)).await?;
    let projects = Project::all().exec(&mut db::db(cx)).await?;
    let mut apps: Vec<RunningApp> = running
        .into_iter()
        .filter_map(|(service_id, sample)| {
            let service = services.iter().find(|service| service.id == service_id)?;
            let project = projects
                .iter()
                .find(|project| project.id == service.project_id)
                .map_or("", |project| project.name.as_str());
            Some(RunningApp {
                name: service.name.clone(),
                project: project.to_owned(),
                load: sample.load,
            })
        })
        .collect();
    apps.sort_by_key(|app| std::cmp::Reverse(app.load.memory));
    Ok(apps)
}

/// oxploy's own database, which sits outside the data folder.
struct Database {
    /// Where it is, or `None` when it is not a SQLite file.
    path: Option<String>,
    /// What it takes, with its write-ahead log.
    bytes: u64,
}

impl Database {
    fn measure() -> Database {
        let files = db::files();
        Database {
            path: files.first().map(|file| {
                std::path::absolute(file)
                    .unwrap_or_else(|_| file.clone())
                    .display()
                    .to_string()
            }),
            bytes: files
                .iter()
                .map(|file| fs::metadata(file).map_or(0, |metadata| metadata.len()))
                .sum(),
        }
    }
}

/// The cards for every metric and for the files, once there are samples.
#[component]
async fn readings(
    samples: Vec<PanelSample>,
    sources: Vec<Source>,
    apps: Vec<RunningApp>,
    cpus: usize,
    footprint: Footprint,
    database: &Database,
) -> Result<impl View> {
    Ok(view! {
        if samples.is_empty() {
            <p class="rounded-xl border border-dashed border-border p-10 text-center text-sm text-muted-foreground">
                "Collecting the first sample…"
            </p>
        } else {
            metrics(
                samples: &samples,
                sources: &sources,
                apps: &apps,
                cpus: cpus,
                footprint: footprint,
                database: database
            )
        }
    })
}

/// The cards for every metric, the apps when they are shown, the files and the table
/// of the latest samples.
#[component]
async fn metrics(
    samples: &[PanelSample],
    sources: &[Source],
    apps: &[RunningApp],
    cpus: usize,
    footprint: Footprint,
    database: &Database,
) -> Result<impl View> {
    let latest = *samples.last().expect("the caller checks for samples");
    let window = Window::new(samples.len());
    let with_apps = sources.len() > 2;
    let lines = |value: fn(&Load) -> f64| -> Vec<Line> {
        sources
            .iter()
            .map(|source| Line {
                color: source.color,
                values: samples.iter().map(|sample| value(&(source.load)(sample))).collect(),
            })
            .collect()
    };
    let cpu = lines(|load| f64::from(load.cpu));
    let memory = lines(|load| load.memory as f64);
    let now = sources
        .iter()
        .map(|source| (source.load)(&latest))
        .fold(Load::default(), |total, load| total + load);
    let disk = [
        samples
            .iter()
            .map(|sample| sample.panel.disk_read + sample.builds.disk_read)
            .collect::<Vec<_>>(),
        samples
            .iter()
            .map(|sample| sample.panel.disk_written + sample.builds.disk_written)
            .collect(),
    ];
    let highest = |lines: &[Line]| {
        lines
            .iter()
            .flat_map(|line| line.values.iter().copied())
            .fold(0.0, f64::max)
    };
    let cpu_max = nice_percent_ceiling(highest(&cpu));
    let memory_max = nice_bytes_ceiling(highest(&memory));
    let disk_max = nice_bytes_ceiling(disk.iter().flatten().copied().fold(0.0, f64::max));
    let builds = match latest.build_processes {
        0 => "no build running".to_owned(),
        1 => "1 build process".to_owned(),
        count => format!("{count} build processes"),
    };
    let disk_read = latest.panel.disk_read + latest.builds.disk_read;
    let disk_written = latest.panel.disk_written + latest.builds.disk_written;
    let entries = move |value: fn(&Load) -> String| -> Vec<(&'static str, &'static str, String)> {
        sources
            .iter()
            .map(|source| (source.color, source.name, value(&(source.load)(&latest))))
            .collect()
    };

    Ok(view! {
        <div class="flex flex-col gap-4" data-samples=(tooltip_data(samples, sources))>
            <div class="grid gap-4 lg:grid-cols-2">
                metric(
                    data: iconify_icon!("lucide:cpu"),
                    title: "CPU",
                    value: format!("{:.1}%", now.cpu),
                    detail: format!("Share of all {cpus} cores · {builds}"),
                    legend(entries: entries(|load| format!("{:.1}%", load.cpu)))
                    chart(
                        window: &window,
                        fields: "cpu",
                        description: if with_apps {
                            "The CPU usage of oxploy and of the apps over the last ten minutes"
                        } else {
                            "oxploy's CPU usage over the last ten minutes"
                        },
                        max: cpu_max,
                        ticks: thirds(cpu_max, |percent| format!("{percent}%")),
                        lines: cpu
                    )
                )
                metric(
                    data: iconify_icon!("lucide:memory-stick"),
                    title: "Memory",
                    value: format_bytes(now.memory),
                    detail: format!("of {} on the server", format_bytes(latest.memory_total)),
                    legend(entries: entries(|load| format_bytes(load.memory)))
                    chart(
                        window: &window,
                        fields: "memory",
                        description: if with_apps {
                            "The memory of oxploy and of the apps over the last ten minutes"
                        } else {
                            "oxploy's memory over the last ten minutes"
                        },
                        max: memory_max,
                        ticks: thirds(memory_max, format_bytes_tick),
                        lines: memory
                    )
                )
                if with_apps {
                    apps_card(apps: apps)
                }
                metric(
                    data: iconify_icon!("lucide:hard-drive-download"),
                    title: "Disk I/O",
                    value: format_rate(disk_read + disk_written),
                    detail: "The panel and its builds".to_owned(),
                    attrs: attributes! { class="lg:col-span-2" },
                    legend(entries: vec![
                        ("text-chart-1", "Read", format_rate(disk_read)),
                        ("text-chart-2", "Write", format_rate(disk_written)),
                    ])
                    chart(
                        window: &window,
                        fields: "read,write",
                        description: "oxploy's disk read and write rates over the last ten minutes",
                        max: disk_max,
                        ticks: thirds(disk_max, format_rate_tick),
                        lines: vec![
                            Line { color: "text-chart-1", values: disk[0].clone() },
                            Line { color: "text-chart-2", values: disk[1].clone() },
                        ]
                    )
                )
                files_card(footprint: footprint, database: database)
            </div>
            latest_readings(samples: samples, sources: sources)
        </div>
    })
}

/// Each app that runs, with its memory and CPU, the one with the most memory first.
#[component]
async fn apps_card(apps: &[RunningApp]) -> Result<impl View> {
    let total = apps.iter().map(|app| app.load.memory).sum();
    let count = match apps.len() {
        0 => "None running".to_owned(),
        1 => "1 running".to_owned(),
        count => format!("{count} running"),
    };

    Ok(view! {
        metric(
            data: iconify_icon!("lucide:boxes"),
            title: "Apps",
            value: format_bytes(total),
            detail: count,
            attrs: attributes! { class="lg:col-span-2" },
            if apps.is_empty() {
                <p class="text-sm text-muted-foreground">"No app runs right now."</p>
            } else {
                <ul class="grid gap-x-8 gap-y-4 sm:grid-cols-2">
                    for app in apps {
                        <li class="flex flex-col gap-1.5">
                            <div class="flex items-baseline justify-between gap-4 text-sm">
                                <span class="min-w-0 truncate">
                                    <span class="font-medium">(&app.name)</span>
                                    <span class="text-muted-foreground">" · " (&app.project)</span>
                                </span>
                                <span class="shrink-0 tabular-nums">
                                    (format_bytes(app.load.memory))
                                    <span class="text-muted-foreground">(format!(" · {:.1}% CPU", app.load.cpu))</span>
                                </span>
                            </div>
                            <div class="h-1.5 overflow-hidden rounded-full bg-chart-3/15" aria-hidden="true">
                                <div
                                    class="h-full rounded-full bg-chart-3"
                                    style=(format!("width:{:.2}%", share(app.load.memory, total) * 100.0))
                                ></div>
                            </div>
                        </li>
                    }
                </ul>
            }
        )
    })
}

/// What oxploy's files take on disk, by kind, largest first.
#[component]
async fn files_card(footprint: Footprint, database: &Database) -> Result<impl View> {
    let database_text = match &database.path {
        Some(path) => format!("oxploy's own projects, services and settings, in {path}."),
        None => "oxploy's own projects, services and settings.".to_owned(),
    };
    let mut kinds = [
        ("Build caches", "Cargo's target folder of each app, so rebuilds only compile what changed.".to_owned(), footprint.build_caches),
        ("Releases", "The code and binary of each app's live release, and of the ones kept to go back to.".to_owned(), footprint.releases),
        ("App storage", "The apps' own data, such as their SQLite databases.".to_owned(), footprint.storage),
        ("Backups", "Copies of the apps' databases.".to_owned(), footprint.backups),
        ("Logs", "What the apps and their deployments printed.".to_owned(), footprint.logs),
        ("Asset caches", "Remote files that Topcoat apps declare, downloaded once.".to_owned(), footprint.asset_caches),
        ("Database", database_text, database.bytes),
    ];
    kinds.sort_by_key(|(_, _, bytes)| std::cmp::Reverse(*bytes));
    let total = footprint.total() + database.bytes;

    Ok(view! {
        metric(
            data: iconify_icon!("lucide:hard-drive"),
            title: "Files",
            value: format_bytes(total),
            detail: "Measured when the page opened".to_owned(),
            attrs: attributes! { class="lg:col-span-2" },
            <p class="text-xs break-all text-muted-foreground">
                "In " <span class="font-mono">(data_dir().display().to_string())</span>
            </p>
            <ul class="grid gap-x-8 gap-y-4 sm:grid-cols-2">
                for (name, text, bytes) in kinds {
                    <li class="flex flex-col gap-1.5">
                        <div class="flex items-baseline justify-between gap-4 text-sm">
                            <span class="font-medium">(name)</span>
                            <span class="tabular-nums">(format_bytes(bytes))</span>
                        </div>
                        <div class="h-1.5 overflow-hidden rounded-full bg-primary/15" aria-hidden="true">
                            <div
                                class="h-full rounded-full bg-primary"
                                style=(format!("width:{:.2}%", share(bytes, total) * 100.0))
                            ></div>
                        </div>
                        <p class="text-xs break-words text-muted-foreground">(text)</p>
                    </li>
                }
            </ul>
        )
    })
}

/// The latest samples as a table, so every value can be read without the charts.
#[component]
async fn latest_readings(samples: &[PanelSample], sources: &[Source]) -> Result<impl View> {
    let rows: Vec<PanelSample> = samples.iter().rev().take(5).copied().collect();
    let mut headings = vec!["Time".to_owned()];
    headings.extend(sources.iter().map(|source| format!("{} CPU", source.name)));
    headings.extend(sources.iter().map(|source| format!("{} memory", source.name)));
    headings.extend(["Disk read".to_owned(), "Disk write".to_owned()]);

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
                            for heading in &headings {
                                <th scope="col" class="px-3 py-2 font-medium whitespace-nowrap">(heading)</th>
                            }
                        </tr>
                    </thead>
                    <tbody>
                        for sample in rows {
                            <tr class="border-t border-border">
                                <td class="px-3 py-2 whitespace-nowrap">(server_clock(sample.at))</td>
                                for source in sources {
                                    <td class="px-3 py-2 whitespace-nowrap">(format!("{:.1}%", (source.load)(&sample).cpu))</td>
                                }
                                for source in sources {
                                    <td class="px-3 py-2 whitespace-nowrap">(format_bytes((source.load)(&sample).memory))</td>
                                }
                                <td class="px-3 py-2 whitespace-nowrap">(format_rate(sample.panel.disk_read + sample.builds.disk_read))</td>
                                <td class="px-3 py-2 whitespace-nowrap">(format_rate(sample.panel.disk_written + sample.builds.disk_written))</td>
                            </tr>
                        }
                    </tbody>
                </table>
            </div>
        </section>
    })
}

/// Every sample's values, formatted, for the readout under the pointer.
fn tooltip_data(samples: &[PanelSample], sources: &[Source]) -> String {
    let rows: Vec<_> = samples
        .iter()
        .map(|sample| {
            let each = |value: fn(&Load) -> String| -> Vec<[String; 2]> {
                sources
                    .iter()
                    .map(|source| [source.name.to_owned(), value(&(source.load)(sample))])
                    .collect()
            };
            json!({
                "time": server_clock(sample.at),
                "cpu": each(|load| format!("{:.1}%", load.cpu)),
                "memory": each(|load| format_bytes(load.memory)),
                "read,write": [
                    ["Read", format_rate(sample.panel.disk_read + sample.builds.disk_read)],
                    ["Write", format_rate(sample.panel.disk_written + sample.builds.disk_written)],
                ],
            })
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}
