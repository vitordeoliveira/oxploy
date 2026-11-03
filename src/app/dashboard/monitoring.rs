//! Usage of the whole server, which only its super admin sees.
//!
//! Each new sample re-renders the readings over the page's WebSocket. The charts are
//! drawn here as SVG; `monitoring.js` adds the readout under the pointer.

use std::time::SystemTime;

use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use topcoat::{
    Result,
    asset::asset,
    context::Cx,
    icon::{IconData, icon, iconify::iconify_icon},
    router::{error::not_found, page},
    runtime::connected,
    view::{Child, View, attributes, class, component, emit, live, view},
};

use super::require_sign_in;
use crate::features::monitoring::{HISTORY, Host, INTERVAL, Monitor, Rates, Sample};
use crate::features::user::Role;

#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    let signed_in = require_sign_in(cx).await?;
    // The numbers describe the whole server, so only the user who owns it sees them.
    if signed_in.user.role != Role::SuperAdmin {
        return Err(not_found().into());
    }
    let host = Monitor::get(cx).host().clone();

    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            <div class="flex flex-col gap-3">
                <div class="flex flex-col gap-1">
                    <h1 class="text-2xl font-semibold tracking-tight">"Server monitoring"</h1>
                    <p class="max-w-2xl text-sm text-muted-foreground">
                        "Usage of the machine oxploy runs on. Apps from every organization share it, so these numbers are the same whichever organization is active."
                    </p>
                </div>
                host_facts(host: &host)
            </div>
            (live! {
                let monitor = Monitor::get(cx);
                let mut sampled = monitor.subscribe();
                loop {
                    let token = emit! { readings(samples: monitor.samples()) }?;
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

/// Which server this is: its name, system, processors and uptime.
#[component]
async fn host_facts(host: &Host) -> Result<impl View> {
    let cpus = if host.cpu_brand.is_empty() {
        format!("{} cores", host.cpu_count)
    } else {
        format!("{} × {}", host.cpu_count, host.cpu_brand)
    };
    let up = SystemTime::now()
        .duration_since(host.booted_at)
        .unwrap_or_default()
        .as_secs();

    Ok(view! {
        <ul class="flex flex-wrap gap-2 text-xs">
            fact(data: iconify_icon!("lucide:server"), label: "Host", value: host.name.clone())
            fact(data: iconify_icon!("lucide:monitor-cog"), label: "System", value: host.os.clone())
            fact(data: iconify_icon!("lucide:cpu"), label: "CPU", value: cpus)
            fact(data: iconify_icon!("lucide:clock"), label: "Up", value: format_duration(up))
        </ul>
    })
}

#[component]
pub(super) async fn fact(data: IconData, label: &'static str, value: String) -> Result<impl View> {
    Ok(view! {
        <li class="flex items-center gap-1.5 rounded-md border border-border bg-muted/60 px-2.5 py-1">
            icon(data: data, size: 14, attrs: attributes! { class="text-muted-foreground" })
            <span class="text-muted-foreground">(label)</span>
            <span class="font-medium">(value)</span>
        </li>
    })
}

/// The cards for every metric and the table of the latest samples, once there are
/// any.
#[component]
async fn readings(samples: Vec<Sample>) -> Result<impl View> {
    Ok(view! {
        if samples.is_empty() {
            <p class="rounded-xl border border-dashed border-border p-10 text-center text-sm text-muted-foreground">
                "Collecting the first sample…"
            </p>
        } else {
            metrics(samples: &samples)
        }
    })
}

/// The cards for every metric and the table of the latest samples.
#[component]
async fn metrics(samples: &[Sample]) -> Result<impl View> {
    let latest = *samples.last().expect("the caller checks for samples");
    // Each sample's rates since the one before it. The first sample has none.
    let rates: Vec<Rates> = std::iter::once(Rates::default())
        .chain(samples.windows(2).map(|pair| pair[1].rates_since(&pair[0])))
        .collect();
    let current = *rates.last().expect("there is a sample");
    let tooltips = tooltip_data(samples, &rates);
    let window = Window::new(samples.len());

    let cpu: Vec<f64> = samples.iter().map(|sample| f64::from(sample.cpu)).collect();
    let memory: Vec<f64> = samples
        .iter()
        .map(|sample| sample.memory_used as f64)
        .collect();
    let disk: Vec<f64> = samples
        .iter()
        .map(|sample| sample.disk_used as f64)
        .collect();
    let disk_rates = [
        rates.iter().map(|rate| rate.disk_read).collect::<Vec<_>>(),
        rates.iter().map(|rate| rate.disk_written).collect(),
    ];
    let network_rates = [
        rates
            .iter()
            .map(|rate| rate.network_received)
            .collect::<Vec<_>>(),
        rates.iter().map(|rate| rate.network_sent).collect(),
    ];
    let disk_max = nice_bytes_ceiling(disk_rates.iter().flatten().copied().fold(0.0, f64::max));
    let network_max =
        nice_bytes_ceiling(network_rates.iter().flatten().copied().fold(0.0, f64::max));
    let memory_total = latest.memory_total as f64;
    let disk_total = latest.disk_total as f64;

    Ok(view! {
        <div class="flex flex-col gap-4" data-samples=(tooltips)>
            <div class="grid gap-4 lg:grid-cols-2">
                metric(
                    data: iconify_icon!("lucide:cpu"),
                    title: "CPU",
                    value: format!("{:.1}%", latest.cpu),
                    detail: "Busy share of all cores".to_owned(),
                    meter(share: f64::from(latest.cpu) / 100.0, alert: "Busy", unit: "busy")
                    chart(
                        window: &window,
                        fields: "cpu",
                        description: "CPU usage over the last ten minutes",
                        max: 100.0,
                        ticks: ["100%".to_owned(), "50%".to_owned(), "0%".to_owned()],
                        lines: vec![Line { color: "text-primary", values: cpu }]
                    )
                )
                metric(
                    data: iconify_icon!("lucide:memory-stick"),
                    title: "Memory",
                    value: format_bytes(latest.memory_used),
                    detail: format!("of {}", format_bytes(latest.memory_total)),
                    meter(share: share(latest.memory_used, latest.memory_total), alert: "Almost full", unit: "used")
                    chart(
                        window: &window,
                        fields: "memory",
                        description: "Memory in use over the last ten minutes",
                        max: memory_total,
                        ticks: thirds(memory_total, |bytes| format_bytes(bytes as u64)),
                        lines: vec![Line { color: "text-primary", values: memory }]
                    )
                )
                metric(
                    data: iconify_icon!("lucide:hard-drive-download"),
                    title: "Disk I/O",
                    value: format_rate(current.disk_read + current.disk_written),
                    detail: format!(
                        "Since boot: read {} · written {}",
                        format_bytes(latest.disk_read),
                        format_bytes(latest.disk_written),
                    ),
                    legend(entries: vec![
                        ("text-chart-1", "Read", format_rate(current.disk_read)),
                        ("text-chart-2", "Write", format_rate(current.disk_written)),
                    ])
                    chart(
                        window: &window,
                        fields: "read,write",
                        description: "Disk read and write rates over the last ten minutes",
                        max: disk_max,
                        ticks: thirds(disk_max, format_rate_tick),
                        lines: vec![
                            Line { color: "text-chart-1", values: disk_rates[0].clone() },
                            Line { color: "text-chart-2", values: disk_rates[1].clone() },
                        ]
                    )
                )
                metric(
                    data: iconify_icon!("lucide:network"),
                    title: "Network",
                    value: format_rate(current.network_received + current.network_sent),
                    detail: format!(
                        "Since boot: in {} · out {}",
                        format_bytes(latest.network_received),
                        format_bytes(latest.network_sent),
                    ),
                    legend(entries: vec![
                        ("text-chart-1", "In", format_rate(current.network_received)),
                        ("text-chart-2", "Out", format_rate(current.network_sent)),
                    ])
                    chart(
                        window: &window,
                        fields: "in,out",
                        description: "Network receive and send rates over the last ten minutes",
                        max: network_max,
                        ticks: thirds(network_max, format_rate_tick),
                        lines: vec![
                            Line { color: "text-chart-1", values: network_rates[0].clone() },
                            Line { color: "text-chart-2", values: network_rates[1].clone() },
                        ]
                    )
                )
                metric(
                    data: iconify_icon!("lucide:hard-drive"),
                    title: "Disk space",
                    value: format_bytes(latest.disk_used),
                    detail: format!(
                        "of {} on / · {} free",
                        format_bytes(latest.disk_total),
                        format_bytes(latest.disk_total.saturating_sub(latest.disk_used)),
                    ),
                    attrs: attributes! { class="lg:col-span-2" },
                    meter(share: share(latest.disk_used, latest.disk_total), alert: "Almost full", unit: "used")
                    chart(
                        window: &window,
                        fields: "disk",
                        description: "Disk space in use over the last ten minutes",
                        max: disk_total,
                        ticks: thirds(disk_total, |bytes| format_bytes(bytes as u64)),
                        lines: vec![Line { color: "text-primary", values: disk }]
                    )
                )
            </div>
            latest_readings(samples: samples, rates: &rates)
        </div>
    })
}

/// A card with a metric's name, its current value and what goes with it.
#[component]
pub(super) async fn metric(
    data: IconData,
    title: &'static str,
    value: String,
    detail: String,
    #[default] mut attrs: topcoat::view::Attributes,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <section
            class=(class!("flex flex-col gap-3 rounded-xl border border-border bg-card p-4", attrs.remove("class")))
            (attrs)
        >
            <div class="flex items-start justify-between gap-4">
                <div class="flex flex-col gap-1">
                    <h2 class="flex items-center gap-1.5 text-sm text-muted-foreground">
                        icon(data: data, size: 14)
                        (title)
                    </h2>
                    <p class="text-2xl font-semibold tracking-tight">(value)</p>
                </div>
                <p class="pt-6 text-right text-xs text-muted-foreground">(detail)</p>
            </div>
            (child)
        </section>
    })
}

/// How much of a resource is taken, from 0 to 1. Past 90% it turns red and says
/// `alert`; the percentage is followed by `unit`, as in `62% used`.
#[component]
pub(super) async fn meter(share: f64, alert: &'static str, unit: &'static str) -> Result<impl View> {
    let share = share.clamp(0.0, 1.0);
    let critical = share >= 0.9;

    Ok(view! {
        <div class="flex items-center gap-3">
            <div
                class=(class!(
                    "h-2 flex-1 overflow-hidden rounded-full",
                    if critical { "bg-destructive/15" } else { "bg-primary/15" },
                ))
                role="meter"
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow=(format!("{:.0}", share * 100.0))
            >
                <div
                    class=(class!("h-full rounded-full", if critical { "bg-destructive" } else { "bg-primary" }))
                    style=(format!("width:{:.2}%", share * 100.0))
                ></div>
            </div>
            <span
                class=(class!(
                    "flex items-center gap-1 text-xs tabular-nums",
                    if critical { "text-destructive" } else { "text-muted-foreground" },
                ))
            >
                if critical {
                    icon(data: iconify_icon!("lucide:triangle-alert"), size: 12)
                    (alert) " · "
                }
                (format!("{:.0}% {unit}", share * 100.0))
            </span>
        </div>
    })
}

/// The names, colors and current values of a chart's series.
#[component]
pub(super) async fn legend(entries: Vec<(&'static str, &'static str, String)>) -> Result<impl View> {
    Ok(view! {
        <ul class="flex flex-wrap gap-x-4 gap-y-1 text-xs">
            for (color, name, value) in entries {
                <li class="flex items-center gap-1.5">
                    <span class=(class!("h-0.5 w-3 rounded-full bg-current", color))></span>
                    <span class="text-muted-foreground">(name)</span>
                    <span class="font-medium tabular-nums">(value)</span>
                </li>
            }
        </ul>
    })
}

/// One series of a chart, colored with a `text-*` class.
pub(super) struct Line {
    pub(super) color: &'static str,
    pub(super) values: Vec<f64>,
}

/// Where the kept samples fall on a chart's ten-minute axis. Until the history fills
/// up, the samples take only its right end.
pub(super) struct Window {
    /// The axis position of the oldest sample.
    start: usize,
}

impl Window {
    pub(super) fn new(samples: usize) -> Self {
        Self {
            start: HISTORY - samples.min(HISTORY),
        }
    }

    /// The SVG x coordinate of sample `index`.
    fn x(&self, index: usize) -> usize {
        self.start + index
    }
}

/// The plot's coordinate space: x spans the history, y spans 0 to 100.
const PLOT_WIDTH: usize = HISTORY - 1;

/// A line chart over the last ten minutes, with gridlines at `ticks` (top, middle and
/// bottom). A single series gets a faint area under its line.
#[component]
pub(super) async fn chart(
    window: &Window,
    fields: &'static str,
    description: &'static str,
    max: f64,
    ticks: [String; 3],
    lines: Vec<Line>,
) -> Result<impl View> {
    let y = |value: f64| {
        let share = if max > 0.0 {
            (value / max).clamp(0.0, 1.0)
        } else {
            0.0
        };
        100.0 - share * 100.0
    };
    let area = lines.len() == 1;
    let drawn: Vec<(&'static str, String, f64)> = lines
        .iter()
        .map(|line| {
            let points: Vec<String> = line
                .values
                .iter()
                .enumerate()
                .map(|(index, value)| format!("{},{:.2}", window.x(index), y(*value)))
                .collect();
            let last = line.values.last().copied().map_or(100.0, y);
            (line.color, points.join(" "), last)
        })
        .collect();
    let start = window.start;

    Ok(view! {
        <div class="grid grid-cols-[auto_1fr] gap-x-3">
            <div class="relative h-32 text-right text-[11px] text-muted-foreground tabular-nums" aria-hidden="true">
                <span class="invisible block">(&ticks[0])</span>
                <span class="absolute top-0 right-0 -translate-y-1/2">(&ticks[0])</span>
                <span class="absolute top-1/2 right-0 -translate-y-1/2">(&ticks[1])</span>
                <span class="absolute right-0 bottom-0 translate-y-1/2">(&ticks[2])</span>
            </div>
            <div
                class="relative h-32 touch-none"
                data-plot=""
                data-start=(start.to_string())
                data-width=(PLOT_WIDTH.to_string())
                data-fields=(fields)
                data-colors=(lines.iter().map(|line| line.color).collect::<Vec<_>>().join(","))
            >
                <svg
                    viewBox=(format!("0 0 {PLOT_WIDTH} 100"))
                    preserveAspectRatio="none"
                    class="absolute inset-0 size-full overflow-visible"
                    role="img"
                    aria-label=(description)
                >
                    <g class="text-border" stroke="currentColor" stroke-width="1">
                        <line x1="0" y1="0" x2=(PLOT_WIDTH.to_string()) y2="0" vector-effect="non-scaling-stroke"></line>
                        <line x1="0" y1="50" x2=(PLOT_WIDTH.to_string()) y2="50" vector-effect="non-scaling-stroke"></line>
                        <line x1="0" y1="100" x2=(PLOT_WIDTH.to_string()) y2="100" vector-effect="non-scaling-stroke"></line>
                    </g>
                    for (color, points, _) in &drawn {
                        <g class=(*color)>
                            if area {
                                <polygon
                                    points=(format!("{start},100 {points} {PLOT_WIDTH},100"))
                                    fill="currentColor"
                                    fill-opacity="0.1"
                                ></polygon>
                            }
                            <polyline
                                points=(points)
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                                stroke-linejoin="round"
                                stroke-linecap="round"
                                vector-effect="non-scaling-stroke"
                            ></polyline>
                        </g>
                    }
                </svg>
                // The latest value of each series, as a dot at the right edge.
                for (color, _, last) in &drawn {
                    <span
                        class=(class!("absolute size-2 -translate-1/2 rounded-full bg-current ring-2 ring-card", *color))
                        style=(format!("left:100%;top:{last:.2}%"))
                        aria-hidden="true"
                    ></span>
                }
            </div>
            <div></div>
            <div class="mt-1.5 flex justify-between text-[11px] text-muted-foreground" aria-hidden="true">
                <span>"10 min ago"</span>
                <span>"now"</span>
            </div>
        </div>
    })
}

/// The latest samples as a table, so every value can be read without the charts.
#[component]
async fn latest_readings(samples: &[Sample], rates: &[Rates]) -> Result<impl View> {
    let rows: Vec<(Sample, Rates)> = samples
        .iter()
        .copied()
        .zip(rates.iter().copied())
        .rev()
        .take(5)
        .collect();

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
                            for heading in ["Time", "CPU", "Memory", "Disk read", "Disk write", "Network in", "Network out"] {
                                <th scope="col" class="px-3 py-2 font-medium whitespace-nowrap">(heading)</th>
                            }
                        </tr>
                    </thead>
                    <tbody>
                        for (sample, rate) in rows {
                            <tr class="border-t border-border">
                                <td class="px-3 py-2 whitespace-nowrap">(server_clock(sample.at))</td>
                                <td class="px-3 py-2 whitespace-nowrap">(format!("{:.1}%", sample.cpu))</td>
                                <td class="px-3 py-2 whitespace-nowrap">(format_bytes(sample.memory_used))</td>
                                <td class="px-3 py-2 whitespace-nowrap">(format_rate(rate.disk_read))</td>
                                <td class="px-3 py-2 whitespace-nowrap">(format_rate(rate.disk_written))</td>
                                <td class="px-3 py-2 whitespace-nowrap">(format_rate(rate.network_received))</td>
                                <td class="px-3 py-2 whitespace-nowrap">(format_rate(rate.network_sent))</td>
                            </tr>
                        }
                    </tbody>
                </table>
            </div>
        </section>
    })
}

/// Every sample's values, formatted, for the readout under the pointer.
fn tooltip_data(samples: &[Sample], rates: &[Rates]) -> String {
    let rows: Vec<_> = samples
        .iter()
        .zip(rates)
        .map(|(sample, rate)| {
            json!({
                "time": server_clock(sample.at),
                "cpu": [["CPU", format!("{:.1}%", sample.cpu)]],
                "memory": [["Memory", format_bytes(sample.memory_used)]],
                "disk": [["Used", format_bytes(sample.disk_used)]],
                "read,write": [["Read", format_rate(rate.disk_read)], ["Write", format_rate(rate.disk_written)]],
                "in,out": [["In", format_rate(rate.network_received)], ["Out", format_rate(rate.network_sent)]],
            })
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}

/// The time of day on the server's clock, such as `18:24:32`.
pub(super) fn server_clock(at: SystemTime) -> String {
    jiff::Timestamp::try_from(at)
        .map(|timestamp| {
            timestamp
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

/// The share of `total` that `used` is, from 0 to 1.
pub(super) fn share(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 / total as f64
    }
}

/// Labels for the top, middle and bottom gridlines of an axis from 0 to `max`.
pub(super) fn thirds(max: f64, format: impl Fn(f64) -> String) -> [String; 3] {
    [format(max), format(max / 2.0), format(0.0)]
}

/// The smallest round byte count at or above `bytes`, or rate at or above a rate, so
/// the axis tops out at 1, 2 or 5 of a unit. A quiet server still gets a 1 KiB/s axis.
pub(super) fn nice_bytes_ceiling(bytes: f64) -> f64 {
    let mut unit = 1024.0;
    while bytes / unit >= 1000.0 {
        unit *= 1024.0;
    }
    let scaled = (bytes / unit).max(1.0);
    let magnitude = 10f64.powf(scaled.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .find(|step| step * magnitude >= scaled)
        .unwrap_or(10.0);
    step * magnitude * unit
}

/// Formats a byte count with a binary unit, such as `2.36 GiB`.
pub(super) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let digits = if unit == 0 || value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    format!("{value:.digits$} {}", UNITS[unit])
}

/// Formats a rate in bytes per second, such as `1.20 MiB/s`.
pub(super) fn format_rate(bytes_per_second: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_second.round() as u64))
}

/// Formats a rate for an axis label, without trailing zeros: `2 MiB/s`.
pub(super) fn format_rate_tick(bytes_per_second: f64) -> String {
    format!("{}/s", format_bytes_tick(bytes_per_second))
}

/// Formats a byte count for an axis label, without trailing zeros: `2 MiB`.
pub(super) fn format_bytes_tick(bytes: f64) -> String {
    let formatted = format_bytes(bytes.round() as u64);
    let (number, unit) = formatted.split_once(' ').unwrap_or((&formatted, ""));
    let number = if number.contains('.') {
        number.trim_end_matches('0').trim_end_matches('.')
    } else {
        number
    };
    format!("{number} {unit}")
}

/// Formats an uptime in seconds by its largest unit: `3 days`, `5 hours`, `12 minutes`.
pub(super) fn format_duration(seconds: u64) -> String {
    let (count, unit) = match seconds {
        0..60 => (seconds, "second"),
        60..3600 => (seconds / 60, "minute"),
        3600..86_400 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}
