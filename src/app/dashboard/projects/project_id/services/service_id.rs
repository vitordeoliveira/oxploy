//! One service, with a tab for each part of deploying it. The tabs have nothing in
//! them yet, only what each one will hold.

pub mod tab;

use std::fmt;
use std::str::FromStr;

use topcoat::{
    Result,
    context::Cx,
    icon::{IconData, icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        href, module_param, page, path_param, route,
    },
    runtime::{Event, signal},
    view::{View, attributes, component, view},
};

use super::super::{ProjectId, current_project};
use crate::app::dashboard::back;
use crate::app::dashboard::projects::{
    DetailsForm, INVALID_DETAILS, created_at, delete_dialog, deploy_status, details_dialog,
    empty_state,
};
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::tabs::{tabs, tabs_content, tabs_list, tabs_trigger};
use crate::features::project::Project;
use crate::features::service::Service;
use tab::Tab;

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
    Domains,
    Deployments,
    Logs,
    Monitoring,
    PreviewDeployments,
    Schedules,
    VolumeBackups,
    Patches,
    Advanced,
}

impl ServiceTab {
    /// Every tab, in the order they are shown.
    const ALL: [ServiceTab; 11] = [
        Self::General,
        Self::Environment,
        Self::Domains,
        Self::Deployments,
        Self::Logs,
        Self::Monitoring,
        Self::PreviewDeployments,
        Self::Schedules,
        Self::VolumeBackups,
        Self::Patches,
        Self::Advanced,
    ];

    /// The tab's segment in the URL.
    fn slug(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Environment => "environment",
            Self::Domains => "domains",
            Self::Deployments => "deployments",
            Self::Logs => "logs",
            Self::Monitoring => "monitoring",
            Self::PreviewDeployments => "preview-deployments",
            Self::Schedules => "schedules",
            Self::VolumeBackups => "volume-backups",
            Self::Patches => "patches",
            Self::Advanced => "advanced",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Environment => "Environment",
            Self::Domains => "Domains",
            Self::Deployments => "Deployments",
            Self::Logs => "Logs",
            Self::Monitoring => "Monitoring",
            Self::PreviewDeployments => "Preview deployments",
            Self::Schedules => "Schedules",
            Self::VolumeBackups => "Volume backups",
            Self::Patches => "Patches",
            Self::Advanced => "Advanced",
        }
    }

    fn icon(self) -> IconData {
        match self {
            Self::General => iconify_icon!("lucide:git-branch"),
            Self::Environment => iconify_icon!("lucide:key-round"),
            Self::Domains => iconify_icon!("lucide:globe"),
            Self::Deployments => iconify_icon!("lucide:rocket"),
            Self::Logs => iconify_icon!("lucide:scroll-text"),
            Self::Monitoring => iconify_icon!("lucide:activity"),
            Self::PreviewDeployments => iconify_icon!("lucide:git-pull-request"),
            Self::Schedules => iconify_icon!("lucide:calendar-clock"),
            Self::VolumeBackups => iconify_icon!("lucide:hard-drive-download"),
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
                "Variables and secrets the binary reads when it starts, such as DATABASE_URL."
            }
            Self::Domains => {
                "The hostnames Caddy sends to this app, with HTTPS certificates it renews on its own."
            }
            Self::Deployments => {
                "Every build of this service with its commit and build log, and a way back to an earlier one."
            }
            Self::Logs => "What the running binary prints, as it prints it.",
            Self::Monitoring => "The CPU, memory and network this service uses.",
            Self::PreviewDeployments => {
                "A temporary copy of the app for each pull request, on its own subdomain."
            }
            Self::Schedules => "Commands that run on a schedule, like cron jobs.",
            Self::VolumeBackups => "Copies of the service's volumes, made on a schedule.",
            Self::Patches => "Changes applied to the repository before each build.",
            Self::Advanced => {
                "The port the app listens on, resource limits, volumes and the command that starts it."
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

/// The service's name and actions above its tabs, with `tab` open.
#[component]
async fn service_view(cx: &Cx, tab: ServiceTab) -> Result<impl View> {
    let (project, service) = current_service(cx).await?;
    let editing = signal(cx, || false);
    let deleting = signal(cx, || false);
    let open_editing = editing.clone();
    let open_deleting = deleting.clone();
    let return_to = tab.url(cx, &project, &service);
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
                            deploy_status()
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
                    empty_state(
                        symbol: tab.icon(),
                        title: tab.label(),
                        text: tab.summary(),
                        <span class="rounded-full border border-border px-2.5 py-0.5 text-xs text-muted-foreground">
                            "Coming soon"
                        </span>
                    )
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
            action: href!(remove, ProjectId(project.id), ServiceId(service.id)).resolve(cx)
        )
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
