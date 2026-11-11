//! Settings of the whole server, which only its super admin sees: the panel's own
//! domain, the time zone it shows times in, the Caddy that serves it and the apps, the
//! version of oxploy, and what it reports to oxploy.com.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{SeeOther, bad_request, see_other},
        href, page, route,
    },
    runtime::{Event, Signal, connected, procedure, shard, signal},
    view::{View, attributes, component, element_id, view},
};

use super::projects::created_at;
use super::proxy::{caddy_notice, resolution_label};
use super::require_server_owner;
use super::update::{update_dialog, version_card};
use crate::app::dashboard::projects::project_id::services::service_id::settings_card;
use crate::components::alert_dialog::alert_dialog;
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::dialog::{dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::components::select::select;
use crate::components::switch::switch;
use crate::db::db;
use crate::features::caddy::{Caddy, CaddyStatus};
use crate::features::domain::{self, PanelDomain};
use crate::features::telemetry::{self, Telemetry};
use crate::features::time_zone::{self, PanelTimeZone};

#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    require_server_owner(cx).await?;
    let caddy = Caddy::get(cx);
    let status = caddy.status();
    let ports = caddy.ports().clone();
    let panel = PanelDomain::get_in(&db(cx)).await?;
    let addresses: Vec<String> = domain::addresses_to_point_at()
        .iter()
        .map(ToString::to_string)
        .collect();
    let removing = signal(cx, || false);
    let open_removing = removing.clone();
    let updating = signal(cx, || false);
    // How many times the version card asked GitHub, which renders it and the dialog again.
    let checks = signal(cx, || 0usize);
    // How many times a switch of usage reporting saved, which renders its card again.
    let reporting_changes = signal(cx, || 0usize);
    let here = href!(page).resolve(cx);
    let time_zone_url = href!(set_time_zone).resolve(cx);
    let time_zone_id = element_id(cx);
    let picked_zone = PanelTimeZone::picked(cx).await?;
    let zones = time_zone::names();
    let server_zone = time_zone::server_zone();
    let local_now = time_zone::now();
    let host_id = element_id(cx);
    let set_url = href!(set_domain).resolve(cx);
    let remove_url = href!(remove_domain).resolve(cx);
    let by_address = ports.panel_urls();
    let panel_text = if by_address.is_empty() {
        format!(
            "The address the panel answers at, over HTTPS through Caddy. Without one, it answers only at {} on this machine.",
            ports.panel
        )
    } else {
        format!(
            "The address the panel answers at, over HTTPS through Caddy. With or without one, it also answers at {}, the server's address.",
            by_address.join(" and ")
        )
    };
    let caddy_state = match &status {
        CaddyStatus::Running => "Running",
        CaddyStatus::Unknown => "Checking…",
        CaddyStatus::Unreachable(_) => "Not running",
        CaddyStatus::Refused(_) => "Refused the routes",
    };

    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            <div class="flex flex-col gap-1">
                <h1 class="text-2xl font-semibold tracking-tight">"Server settings"</h1>
                <p class="max-w-2xl text-sm text-muted-foreground">
                    "Settings of the machine oxploy runs on. They are the same whichever organization is active."
                </p>
            </div>
            settings_card(
                title: "Panel domain",
                text: &panel_text,
                caddy_notice(status: &status, admin: &ports.admin)
                if let Some(panel) = &panel {
                    <div class="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border p-4">
                        <div class="flex min-w-0 flex-col gap-1.5">
                            <a
                                href=(panel.url())
                                class="flex min-w-0 items-center gap-1.5 font-medium hover:underline"
                            >
                                icon(data: iconify_icon!("lucide:lock"), size: 14, attrs: attributes! { class="shrink-0 text-muted-foreground" })
                                <span class="truncate">(&panel.host)</span>
                            </a>
                            <div class="text-xs">panel_dns()</div>
                        </div>
                        button(
                            variant: ButtonVariant::Outline,
                            size: ButtonSize::Sm,
                            attrs: attributes! { type="button" @click=$(|_e: Event| open_removing.set(true)) },
                            "Remove"
                        )
                    </div>
                }
                <form method="post" action=(&set_url) class="flex flex-col gap-4 sm:flex-row sm:items-end">
                    field(
                        attrs: attributes! { class="flex-1" },
                        field_label(attrs: attributes! { for=(&host_id) }, (if panel.is_some() { "New domain" } else { "Domain" }))
                        input(
                            attrs: attributes! {
                                id=(&host_id)
                                name="host"
                                placeholder="panel.example.com"
                                maxlength="253"
                                autocomplete="off"
                                autocapitalize="off"
                                spellcheck="false"
                                required=""
                            }
                        )
                    )
                    button(attrs: attributes! { type="submit" }, (if panel.is_some() { "Change domain" } else { "Set domain" }))
                </form>
                field_description(
                    "Point its A or AAAA record to this server"
                    if addresses.is_empty() {
                        "."
                    } else {
                        ": " <span class="font-mono break-all text-foreground">(addresses.join(", "))</span> "."
                    }
                    " Caddy gets its certificate from Let's Encrypt, or its own for names under localhost. Signing in to the panel needs HTTPS away from localhost, so it has no plain HTTP."
                )
            )
            settings_card(
                title: "Time zone",
                text: "The time zone the panel shows times in: its clock, its charts and dates, and the times oxploy writes in the apps' logs and backups. The apps keep the server's own.",
                <form method="post" action=(&time_zone_url) class="flex flex-col gap-4 sm:flex-row sm:items-end">
                    field(
                        attrs: attributes! { class="flex-1" },
                        field_label(attrs: attributes! { for=(&time_zone_id) }, "Time zone")
                        select(
                            attrs: attributes! { id=(&time_zone_id) name="time_zone" },
                            <option value="" selected=(picked_zone.is_none().then_some(""))>
                                "The server's, " (&server_zone)
                            </option>
                            #[key(zone.clone())]
                            for zone in &zones {
                                <option value=(zone) selected=((picked_zone.as_deref() == Some(zone.as_str())).then_some(""))>
                                    (zone)
                                </option>
                            }
                        )
                    )
                    button(attrs: attributes! { type="submit" }, "Save time zone")
                </form>
                field_description(
                    "It is " (local_now.strftime("%H:%M").to_string()) " there now, "
                    (local_now.strftime("%Y-%m-%d").to_string()) ", UTC" (local_now.strftime("%:z").to_string()) "."
                )
            )
            settings_card(
                title: "Caddy",
                text: "The web server in front of the panel and the apps. It runs on its own, as a service of this machine, and oxploy gives it every route.",
                <dl class="grid grid-cols-1 gap-x-6 gap-y-3 text-sm sm:grid-cols-[10rem_minmax(0,1fr)]">
                    <dt class="font-medium">"State"</dt>
                    <dd class="text-muted-foreground">(caddy_state)</dd>
                    <dt class="font-medium">"Admin API"</dt>
                    <dd class="font-mono text-muted-foreground">(&ports.admin)</dd>
                    <dt class="font-medium">"Ports"</dt>
                    <dd class="text-muted-foreground">(format!("{} for HTTP, {} for HTTPS", ports.http, ports.https))</dd>
                </dl>
                field_description(
                    "Change them with OXPLOY_CADDY_ADMIN, OXPLOY_HTTP_PORT and OXPLOY_HTTPS_PORT. The Caddy belongs to oxploy, which replaces its whole configuration."
                )
            )
            version_card(open: updating.clone(), checks: checks.clone())
            usage_reporting_card(changes: reporting_changes.clone())
        </div>
        update_dialog(open: updating.clone(), checks: checks.clone(), return_to: here.clone())
        if let Some(panel) = &panel {
            remove_dialog(open: &removing, host: &panel.host, panel: &ports.panel, action: &remove_url)
        }
    })
}

/// Whether oxploy tells oxploy.com once a day that it runs, and what it runs, with the
/// report as it would go now. Each switch saves at once. A shard answers requests of its
/// own, so it checks access itself.
#[shard]
async fn usage_reporting_card(cx: &Cx, changes: Signal<usize>) -> Result<impl View> {
    require_server_owner(cx).await?;
    let _ = changes.get();
    let telemetry = Telemetry::get_in(&db(cx)).await?;
    let off_by = telemetry::turned_off();
    let report = telemetry.report(&db(cx)).await?;
    let report = serde_json::to_string_pretty(&report).unwrap_or_default();
    let forced_off = off_by.is_some();
    let sending = !forced_off && telemetry.reports;
    let reports = signal(cx, || telemetry.reports);
    let shares = signal(cx, || telemetry.shares_usage);
    let reports_id = element_id(cx);
    let shares_id = element_id(cx);
    let changed = changes.clone();
    let changed_too = changes.clone();
    let shares_off = reports.clone();
    // A failure since the last report that went out.
    let failure = telemetry
        .failed_at
        .filter(|failed| telemetry.sent_at.is_none_or(|sent| *failed > sent))
        .zip(telemetry.error.clone());

    Ok(view! {
        settings_card(
            title: "Usage reporting",
            text: "Once a day, oxploy tells oxploy.com that it runs, with its version and a random ID that says nothing about this server or you. It is how the project knows how many servers run oxploy, and which versions they are on.",
            if let Some(by) = off_by {
                <p class="flex items-start gap-2 rounded-lg bg-muted px-4 py-3 text-sm">
                    icon(data: iconify_icon!("lucide:circle-off"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0 text-muted-foreground" })
                    <span>"Turned off by " <span class="font-mono">(by)</span> ", so oxploy sends nothing, whatever is picked here."</span>
                </p>
            }
            <div class="flex flex-col divide-y divide-border rounded-lg border border-border">
                <div class="flex items-start justify-between gap-4 p-4">
                    <div class="flex flex-col gap-0.5">
                        <label for=(&reports_id) class="text-sm font-medium">"Report that this server runs"</label>
                        <p class="text-xs text-muted-foreground">"Its version, its machine, such as x86_64-linux, and its random ID."</p>
                    </div>
                    switch(
                        attrs: attributes! {
                            id=(&reports_id)
                            class="mt-0.5"
                            disabled=(forced_off.then_some(""))
                            :checked=$(reports.get())
                            @change=$(async |_e: Event| {
                                let saved = set_reporting(!reports.get()).await;
                                reports.set(saved);
                                changed.increment();
                            })
                        }
                    )
                </div>
                <div class="flex items-start justify-between gap-4 p-4">
                    <div class="flex flex-col gap-0.5">
                        <label for=(&shares_id) class="text-sm font-medium">"Share what it runs"</label>
                        <p class="text-xs text-muted-foreground">
                            "Adds counts, such as how many apps, domains and deployments it has, and which ways it builds them. Never their names."
                        </p>
                    </div>
                    switch(
                        attrs: attributes! {
                            id=(&shares_id)
                            class="mt-0.5"
                            :disabled=$(if forced_off { true } else { !shares_off.get() })
                            :checked=$(shares.get())
                            @change=$(async |_e: Event| {
                                let saved = set_sharing(!shares.get()).await;
                                shares.set(saved);
                                changed_too.increment();
                            })
                        }
                    )
                </div>
            </div>
            <p role="status" class="text-sm text-muted-foreground">
                if !sending {
                    "Nothing is sent."
                } else if let Some((failed, error)) = &failure {
                    <span class="text-destructive">"The last report failed " created_at(seconds: *failed) ": " (error) "."</span>
                    " oxploy tries again within the hour."
                } else if let Some(sent) = telemetry.sent_at {
                    "Last sent " created_at(seconds: sent) ". The next goes out a day after it."
                } else {
                    "Not sent yet. oxploy sends one within the hour."
                }
            </p>
            <details class="group rounded-lg border border-border">
                <summary class="flex cursor-pointer items-center gap-2 px-4 py-3 text-sm font-medium select-none">
                    icon(data: iconify_icon!("lucide:chevron-right"), size: 14, attrs: attributes! { class="shrink-0 transition-transform group-open:rotate-90" })
                    if sending { "What it sends now" } else { "What it would send" }
                </summary>
                <pre class="overflow-x-auto border-t border-border bg-muted/40 px-4 py-3 font-mono text-xs">(report)</pre>
            </details>
            field_description(
                "Turn it off for good with OXPLOY_TELEMETRY=off, or DO_NOT_TRACK=1, in oxploy's environment. "
                <a href="https://oxploy.com/docs/usage-reporting" class="font-medium text-foreground underline underline-offset-4">"What it sends, and what oxploy.com keeps"</a>
            )
        )
    })
}

/// Turns usage reporting on or off, and returns whether it is on.
#[procedure]
async fn set_reporting(cx: &Cx, on: bool) -> Result<bool> {
    require_server_owner(cx).await?;
    Telemetry::set_reports(&db(cx), on).await?;
    Ok(on)
}

/// Turns sharing what the server runs on or off, and returns whether it is on.
#[procedure]
async fn set_sharing(cx: &Cx, on: bool) -> Result<bool> {
    require_server_owner(cx).await?;
    Telemetry::set_shares_usage(&db(cx), on).await?;
    Ok(on)
}

/// Where the panel's domain points, looked up once the page is connected. A shard
/// answers requests of its own, so it checks access itself.
#[shard]
async fn panel_dns(cx: &Cx) -> Result<impl View> {
    require_server_owner(cx).await?;
    let panel = PanelDomain::get_in(&db(cx)).await?;
    let resolution = match &panel {
        Some(panel) if connected(cx) => Some(domain::resolve(&panel.host).await),
        _ => None,
    };

    Ok(view! {
        if let Some(panel) = &panel {
            resolution_label(host: &panel.host, resolution: resolution.as_ref())
        }
    })
}

/// A dialog that confirms taking the panel's domain away.
#[component]
async fn remove_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    host: &str,
    panel: &str,
    action: &str,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);

    Ok(view! {
        alert_dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Remove " (host))
                    dialog_description(
                        "The panel stops answering at it. If you opened the panel there, open it again at "
                        <span class="font-mono">(panel)</span>
                        " on this machine."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(action)>
                        button(
                            variant: ButtonVariant::Destructive,
                            attrs: attributes! { type="submit" },
                            "Remove domain"
                        )
                    </form>
                )
            )
        )
    })
}

/// The time zone, as the form sends it: empty for the server's own.
#[derive(Deserialize)]
struct TimeZoneForm {
    time_zone: String,
}

/// Picks the time zone the panel shows times in.
#[route(POST "./time-zone")]
async fn set_time_zone(cx: &Cx, Form(form): Form<TimeZoneForm>) -> Result<SeeOther> {
    require_server_owner(cx).await?;
    if !PanelTimeZone::pick(cx, &form.time_zone).await? {
        return Err(bad_request("Choose a time zone from the list").into());
    }
    Ok(see_other(href!(page).resolve(cx)))
}

/// The panel's domain, as the form sends it.
#[derive(Deserialize)]
struct DomainForm {
    host: String,
}

/// Gives the panel a domain, in place of the one it had.
#[route(POST "./domain")]
async fn set_domain(cx: &Cx, Form(form): Form<DomainForm>) -> Result<SeeOther> {
    require_server_owner(cx).await?;
    if let Err(problem) = PanelDomain::set(cx, &form.host).await? {
        return Err(bad_request(problem).into());
    }
    Caddy::get(cx).changed();
    Ok(see_other(href!(page).resolve(cx)))
}

/// Takes the panel's domain away.
#[route(POST "./remove-domain")]
async fn remove_domain(cx: &Cx) -> Result<SeeOther> {
    require_server_owner(cx).await?;
    PanelDomain::remove(cx).await?;
    Caddy::get(cx).changed();
    Ok(see_other(href!(page).resolve(cx)))
}
