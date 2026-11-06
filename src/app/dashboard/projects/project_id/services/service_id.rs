//! One service, with a tab for each part of deploying it. The tabs that have nothing
//! yet say what they will hold.

mod general;
mod history;
mod hosts;
mod mcp_server;
mod output;
mod process;
mod stored;
pub mod tab;
mod usage;
mod variables;

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

use topcoat::{
    Result,
    context::Cx,
    icon::{IconData, icon, iconify::iconify_icon},
    router::{
        Uri,
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        header::HOST,
        href, module_param, page, path_param,
        request::{headers, uri},
        route,
    },
    runtime::{Event, connected, shard, signal},
    view::{Child, View, attributes, component, emit, live, view},
};

use super::super::{ProjectId, current_project};
use crate::app::dashboard::{back, require_organization};
use crate::app::dashboard::projects::{
    DetailsForm, INVALID_DETAILS, created_at, delete_dialog, deploy_status, details_dialog,
    empty_state,
};
use crate::components::button::{ButtonSize, ButtonVariant, button, button_variants};
use crate::components::tabs::{tabs, tabs_content, tabs_list, tabs_trigger};
use crate::features::deploy::{AppStatus, Deployer};
use crate::features::domain::Domain;
use crate::features::project::Project;
use crate::features::service::Service;
use general::general_tab;
use history::deployments_tab;
use hosts::domains_tab;
use mcp_server::mcp_tab;
use output::logs_tab;
use process::advanced_tab;
use stored::storage_tab;
use tab::Tab;
use usage::monitoring_tab;
use variables::{Draft, environment_tab};

/// How often the service's status refreshes on its page.
const STATUS_INTERVAL: Duration = Duration::from_secs(2);

module_param!(pub service_id: u64, error = not_found);

/// Returns the service in the URL with its project, or a not found error unless both
/// belong to the organization the browser is working in.
pub async fn current_service(cx: &Cx) -> Result<(Project, Service)> {
    let project = current_project(cx).await?;
    let id = *path_param::<ServiceId>(cx)?;
    let service = Service::find_in(cx, project.id, id)
        .await?
        .ok_or_not_found()?;
    Ok((project, service))
}

/// A part of a service, shown in a tab of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceTab {
    General,
    Environment,
    Storage,
    Domains,
    Deployments,
    Logs,
    Monitoring,
    Mcp,
    PreviewDeployments,
    Schedules,
    Patches,
    Advanced,
}

impl ServiceTab {
    /// Every tab, in the order they are shown.
    const ALL: [ServiceTab; 12] = [
        Self::General,
        Self::Environment,
        Self::Storage,
        Self::Domains,
        Self::Deployments,
        Self::Logs,
        Self::Monitoring,
        Self::Mcp,
        Self::PreviewDeployments,
        Self::Schedules,
        Self::Patches,
        Self::Advanced,
    ];

    /// The tab's segment in the URL.
    fn slug(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Environment => "environment",
            Self::Storage => "storage",
            Self::Domains => "domains",
            Self::Deployments => "deployments",
            Self::Logs => "logs",
            Self::Monitoring => "monitoring",
            Self::Mcp => "mcp",
            Self::PreviewDeployments => "preview-deployments",
            Self::Schedules => "schedules",
            Self::Patches => "patches",
            Self::Advanced => "advanced",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Environment => "Environment",
            Self::Storage => "Storage",
            Self::Domains => "Domains",
            Self::Deployments => "Deployments",
            Self::Logs => "Logs",
            Self::Monitoring => "Monitoring",
            Self::Mcp => "MCP",
            Self::PreviewDeployments => "Preview deployments",
            Self::Schedules => "Schedules",
            Self::Patches => "Patches",
            Self::Advanced => "Advanced",
        }
    }

    fn icon(self) -> IconData {
        match self {
            Self::General => iconify_icon!("lucide:git-branch"),
            Self::Environment => iconify_icon!("lucide:key-round"),
            Self::Storage => iconify_icon!("lucide:database"),
            Self::Domains => iconify_icon!("lucide:globe"),
            Self::Deployments => iconify_icon!("lucide:rocket"),
            Self::Logs => iconify_icon!("lucide:scroll-text"),
            Self::Monitoring => iconify_icon!("lucide:activity"),
            Self::Mcp => iconify_icon!("lucide:plug"),
            Self::PreviewDeployments => iconify_icon!("lucide:git-pull-request"),
            Self::Schedules => iconify_icon!("lucide:calendar-clock"),
            Self::Patches => iconify_icon!("lucide:file-diff"),
            Self::Advanced => iconify_icon!("lucide:sliders-horizontal"),
        }
    }

    /// What the tab will hold.
    fn summary(self) -> &'static str {
        match self {
            Self::General => {
                "The repository and branch to deploy, how cargo builds the binary, and the buttons to deploy, restart and stop it."
            }
            Self::Environment => {
                "Variables passed to cargo while it builds and to the binary when it runs."
            }
            Self::Storage => "The folder that keeps the app's data, its SQLite databases and their backups.",
            Self::Domains => {
                "The hostnames Caddy sends to this app, with HTTPS certificates it renews on its own."
            }
            Self::Deployments => {
                "Every build of this service with its commit and build log, and a way back to an earlier one."
            }
            Self::Logs => "What the running binary prints, as it prints it.",
            Self::Monitoring => "The CPU, memory and disk the app takes while it runs.",
            Self::Mcp => "The MCP server the app serves, and which of its tools reach this organization's MCP.",
            Self::PreviewDeployments => {
                "A temporary copy of the app for each pull request, on its own subdomain."
            }
            Self::Schedules => "Commands that run on a schedule, like cron jobs.",
            Self::Patches => "Changes applied to the repository before each build.",
            Self::Advanced => {
                "How oxploy runs the app, and what runs after each build."
            }
        }
    }

    /// The URL of the tab on a service's page.
    fn url(self, cx: &Cx, project: &Project, service: &Service) -> String {
        let (project, service) = (ProjectId(project.id), ServiceId(service.id));
        match self {
            Self::General => href!(page, project, service).resolve(cx),
            tab => href!(tab::page, project, service, Tab(tab)).resolve(cx),
        }
    }
}

impl fmt::Display for ServiceTab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

/// The URL segment did not name a tab.
#[derive(Debug)]
pub struct UnknownTab;

impl fmt::Display for UnknownTab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown tab")
    }
}

impl std::error::Error for UnknownTab {}

impl FromStr for ServiceTab {
    type Err = UnknownTab;

    /// Parses a tab's slug. General has no segment of its own: it is the service's page.
    fn from_str(slug: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .filter(|tab| *tab != Self::General)
            .find(|tab| tab.slug() == slug)
            .ok_or(UnknownTab)
    }
}

/// The service's General tab.
#[page]
pub async fn page() -> Result<impl View> {
    Ok(view! { service_view(tab: ServiceTab::General) })
}

/// The service's name and actions above its tabs, with `tab` open. `draft` holds
/// variables that could not be saved, to show again on the Environment tab.
#[component]
async fn service_view(cx: &Cx, tab: ServiceTab, #[default] draft: Option<&Draft>) -> Result<impl View> {
    let (project, service) = current_service(cx).await?;
    let editing = signal(cx, || false);
    let deleting = signal(cx, || false);
    let open_editing = editing.clone();
    let open_deleting = deleting.clone();
    let return_to = tab.url(cx, &project, &service);
    let local = panel_is_local(cx);
    let (project_id, service_id) = (project.id, service.id);
    let links: Vec<_> = ServiceTab::ALL
        .into_iter()
        .map(|other| (other, other.url(cx, &project, &service)))
        .collect();

    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            <div class="flex flex-wrap items-start justify-between gap-4">
                <div class="flex min-w-0 items-start gap-3">
                    <span class="flex size-11 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary">
                        icon(data: iconify_icon!("lucide:box"), size: 22)
                    </span>
                    <div class="flex min-w-0 flex-col gap-1">
                        <div class="flex min-w-0 flex-wrap items-center gap-2">
                            <h1 class="truncate text-2xl font-semibold tracking-tight">(&service.name)</h1>
                            service_status(project_id: project_id, service_id: service_id, local: local)
                        </div>
                        <p class="flex flex-wrap items-center gap-x-2 text-sm text-muted-foreground">
                            <span class="font-mono text-xs" title="App name">(&service.app_name)</span>
                            <span aria-hidden="true">"·"</span>
                            <span>"Created " created_at(seconds: service.created_at)</span>
                        </p>
                        if let Some(description) = &service.description {
                            <p class="max-w-2xl text-sm text-muted-foreground">(description)</p>
                        }
                    </div>
                </div>
                <div class="flex items-center gap-2">
                    button(
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Icon,
                        attrs: attributes! {
                            type="button"
                            title="Edit service"
                            aria-label="Edit service"
                            @click=$(|_e: Event| open_editing.set(true))
                        },
                        icon(data: iconify_icon!("lucide:pencil"))
                    )
                    button(
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Icon,
                        attrs: attributes! {
                            type="button"
                            class="hover:text-destructive"
                            title="Delete service"
                            aria-label="Delete service"
                            @click=$(|_e: Event| open_deleting.set(true))
                        },
                        icon(data: iconify_icon!("lucide:trash-2"))
                    )
                </div>
            </div>
            tabs(
                attrs: attributes! { class="flex-1" },
                // The tabs scroll sideways when they do not fit.
                <nav aria-label="Service" class="overflow-x-auto">
                    tabs_list(
                        #[key(other.slug())]
                        for (other, url) in &links {
                            tabs_trigger(
                                active: *other == tab,
                                attrs: attributes! { href=(url) },
                                (other.label())
                            )
                        }
                    )
                </nav>
                tabs_content(
                    attrs: attributes! { class="flex flex-1 flex-col" },
                    match tab {
                        ServiceTab::General => {
                            general_tab(project: &project, service: &service, here: &return_to)
                        }
                        ServiceTab::Environment => {
                            environment_tab(service: &service, here: &return_to, draft: draft)
                        }
                        ServiceTab::Storage => {
                            storage_tab(project: &project, service: &service)
                        }
                        ServiceTab::Domains => {
                            domains_tab(project: &project, service: &service)
                        }
                        ServiceTab::Deployments => {
                            deployments_tab(project: &project, service: &service, here: &return_to)
                        }
                        ServiceTab::Logs => {
                            logs_tab(service: &service)
                        }
                        ServiceTab::Monitoring => {
                            monitoring_tab(service: &service)
                        }
                        ServiceTab::Mcp => {
                            mcp_tab(project: &project, service: &service)
                        }
                        ServiceTab::Advanced => {
                            advanced_tab(project: &project, service: &service, here: &return_to)
                        }
                        _ => {
                            empty_state(
                                symbol: tab.icon(),
                                title: tab.label(),
                                text: tab.summary(),
                                <span class="rounded-full border border-border px-2.5 py-0.5 text-xs text-muted-foreground">
                                    "Coming soon"
                                </span>
                            )
                        }
                    }
                )
            )
        </div>
        details_dialog(
            open: &editing,
            title: "Edit service",
            description: "Change the name and description. The app name stays the same.",
            action: href!(edit, ProjectId(project.id), ServiceId(service.id)).resolve(cx),
            details: Some((&service.name, service.description.as_deref())),
            placeholder: "API",
            submit: "Save changes",
            return_to: Some(&return_to)
        )
        delete_dialog(
            open: &deleting,
            title: "Delete service",
            name: &service.name,
            consequence: " with the files in its storage".to_owned(),
            action: href!(remove, ProjectId(project.id), ServiceId(service.id)).resolve(cx)
        )
    })
}

/// The service's status, refreshed every [`STATUS_INTERVAL`].
///
/// It is a shard so that the connection that refreshes it is its own: a page connected
/// as a whole renders again as a plain `GET`, which would drop a form the page shows
/// back with a problem, such as the Environment tab's. A shard answers requests of its
/// own, so it checks access itself.
#[shard]
async fn service_status(cx: &Cx, project_id: u64, service_id: u64, local: bool) -> Result<impl View> {
    let organization = require_organization(cx).await?;
    let project = Project::find_in(cx, organization.id, project_id)
        .await?
        .ok_or_not_found()?;

    Ok(view! {
        (live! {
            loop {
                let service = Service::find_in(cx, project.id, service_id)
                    .await?
                    .ok_or_not_found()?;
                let status = Deployer::get(cx).status(&service);
                let domains = Domain::of_service(cx, service.id).await?;
                // A domain reaches the app from anywhere. Without one, the app listens
                // on 127.0.0.1, which only a browser on the same machine reaches.
                let open = match Domain::default_of(&domains, service.default_domain_id) {
                    Some(domain) if status == AppStatus::Running => Some(domain.url()),
                    _ => service
                        .port
                        .filter(|_| local && status == AppStatus::Running)
                        .map(|port| format!("http://127.0.0.1:{port}")),
                };
                let token = emit! { status_line(status: status, open: open.as_deref()) }?;
                if !connected(cx) {
                    break Ok(token);
                }
                tokio::time::sleep(STATUS_INTERVAL).await;
            }
        })
    })
}

/// The service's status, with a link that opens the app when the browser can reach it.
#[component]
async fn status_line(status: AppStatus, open: Option<&str>) -> Result<impl View> {
    Ok(view! {
        <span class="inline-flex items-center gap-2">
            deploy_status(status: status)
            if let Some(open) = open {
                <a
                    href=(open)
                    target="_blank"
                    rel="noreferrer"
                    class=(button_variants(ButtonVariant::Outline, ButtonSize::Sm))
                >
                    "Open"
                    icon(data: iconify_icon!("lucide:external-link"), size: 14)
                </a>
            }
        </span>
    })
}

/// Whether the browser reached the panel on the machine it runs on, so it can also
/// open an app's 127.0.0.1 address.
fn panel_is_local(cx: &Cx) -> bool {
    let host = match uri(cx).authority() {
        Some(authority) => authority.host().to_owned(),
        None => headers(cx)
            .get(HOST)
            .and_then(|host| host.to_str().ok())
            .and_then(|host| format!("http://{host}").parse::<Uri>().ok())
            .and_then(|uri| uri.host().map(str::to_owned))
            .unwrap_or_default(),
    };
    let address = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || address.parse::<IpAddr>().is_ok_and(|address| address.is_loopback())
}

/// The end of a log, which stays scrolled to its newest line as lines arrive. Lines
/// from oxploy, which start with `==>`, stand out, as do errors and warnings.
#[component]
async fn log_lines(log: &str, empty: &str) -> Result<impl View> {
    Ok(view! {
        if log.is_empty() {
            <p class="rounded-lg bg-muted px-4 py-6 text-center text-sm text-muted-foreground">(empty)</p>
        } else {
            // Reversed, so the browser keeps the view at the bottom as the log grows.
            <div class="flex max-h-[36rem] min-h-48 flex-col-reverse overflow-auto rounded-lg bg-muted p-4">
                <div class="font-mono text-xs leading-relaxed">
                    #[key(index)]
                    for (index, line) in log.lines().enumerate() {
                        <div class=(log_line_class(line))>(line)</div>
                    }
                </div>
            </div>
        }
    })
}

/// How a log line looks, by what it says.
fn log_line_class(line: &str) -> &'static str {
    let line = line.trim_start();
    if line.starts_with("==> ") {
        "min-h-[1.625em] font-semibold whitespace-pre-wrap break-all text-foreground"
    } else if line.starts_with("error") {
        "min-h-[1.625em] whitespace-pre-wrap break-all text-destructive"
    } else if line.starts_with("warning") {
        "min-h-[1.625em] whitespace-pre-wrap break-all text-amber-700 dark:text-amber-400"
    } else {
        "min-h-[1.625em] whitespace-pre-wrap break-all text-foreground/75"
    }
}

/// A card with a heading over one group of settings.
#[component]
pub(crate) async fn settings_card(title: &str, text: &str, #[default] child: Child<'_>) -> Result<impl View> {
    Ok(view! {
        <section class="flex flex-col rounded-xl border border-border bg-card">
            <header class="flex flex-col gap-0.5 border-b border-border px-5 py-4">
                <h2 class="font-semibold">(title)</h2>
                <p class="text-sm text-muted-foreground">(text)</p>
            </header>
            <div class="flex flex-col gap-5 p-5">(child)</div>
        </section>
    })
}

/// Changes the name and description of the service.
#[route(POST "./edit")]
async fn edit(cx: &Cx, Form(form): Form<DetailsForm>) -> Result<SeeOther> {
    let (_, mut service) = current_service(cx).await?;
    if !service.edit(cx, &form.details()).await? {
        return Err(bad_request(INVALID_DETAILS).into());
    }
    Ok(back(cx, &form.return_to))
}

/// Deletes the service, then shows the rest of its project.
#[route(POST "./delete")]
async fn remove(cx: &Cx) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    service.remove(cx).await?;
    Ok(see_other(
        href!(super::super::page, ProjectId(project.id)).resolve(cx),
    ))
}
