//! The Deployments tab: the service's deployments, and the log of one of them as it
//! grows.

use std::time::Duration;

use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        error::RouterErrorExt,
        href, query_params,
    },
    runtime::connected,
    view::{View, attributes, component, emit, live, view},
};

use super::super::super::ProjectId;
use super::{ServiceId, general, log_lines};
use crate::app::dashboard::projects::{created_at, empty_state};
use crate::components::button::button;
use crate::features::app_files::{AppFiles, tail};
use crate::features::deployment::{Deployment, DeploymentStatus};
use crate::features::project::{Project, now};
use crate::features::service::Service;

/// How many lines of a deployment's log the tab shows, from its end.
const LOG_LINES: usize = 1500;

/// How often the tab refreshes while a deployment is in progress, and otherwise.
const BUSY_INTERVAL: Duration = Duration::from_millis(700);
const IDLE_INTERVAL: Duration = Duration::from_secs(3);

/// Which deployment the tab shows: the one in `?deployment=`, or the latest.
#[query_params(error = bad_request)]
struct Shown {
    deployment: Option<u64>,
}

/// The service's deployments, refreshed while the page is open.
#[component]
pub async fn deployments_tab(
    cx: &Cx,
    project: &Project,
    service: &Service,
    here: &str,
) -> Result<impl View> {
    let shown = query_params::<Shown>(cx)?.deployment;
    let (project_id, service_id) = (project.id, service.id);
    let files = AppFiles::of(&service.app_name);
    let here = here.to_owned();
    let deploy_url =
        href!(general::deploy, ProjectId(project.id), ServiceId(service.id)).resolve(cx);

    Ok(view! {
        (live! {
            loop {
                let service = Service::find_in(cx, project_id, service_id)
                    .await?
                    .ok_or_not_found()?;
                let deployments = Deployment::latest_of(cx, service_id).await?;
                let in_progress = deployments.iter().any(|deployment| deployment.status.is_active());
                let token = emit! {
                    deployments_view(
                        service: &service,
                        deployments: &deployments,
                        shown: shown,
                        files: &files,
                        here: &here,
                        deploy_url: &deploy_url
                    )
                }?;
                if !connected(cx) {
                    break Ok(token);
                }
                tokio::time::sleep(if in_progress { BUSY_INTERVAL } else { IDLE_INTERVAL }).await;
            }
        })
    })
}

/// The list of deployments beside the log of the one shown.
#[component]
async fn deployments_view(
    service: &Service,
    deployments: &[Deployment],
    shown: Option<u64>,
    files: &AppFiles,
    here: &str,
    deploy_url: &str,
) -> Result<impl View> {
    let current = shown
        .and_then(|id| deployments.iter().find(|deployment| deployment.id == id))
        .or(deployments.first());
    let log = current.map_or_else(String::new, |deployment| {
        tail(&files.deploy_log(deployment.id), LOG_LINES)
    });

    Ok(view! {
        if deployments.is_empty() {
            empty_state(
                symbol: iconify_icon!("lucide:rocket"),
                title: "No deployments yet",
                text: "Deploying fetches the branch, builds it with cargo and starts it. Each deployment shows up here with its log.",
                <form method="post" action=(deploy_url)>
                    button(
                        attrs: attributes! { type="submit" },
                        icon(data: iconify_icon!("lucide:rocket"))
                        "Deploy"
                    )
                </form>
            )
        } else {
            <div class="grid grid-cols-1 gap-4 lg:grid-cols-[20rem_minmax(0,1fr)]">
                <nav aria-label="Deployments">
                    <ul class="flex flex-col gap-1">
                        #[key(deployment.id)]
                        for deployment in deployments {
                            let selected = current.is_some_and(|current| current.id == deployment.id);
                            let live = service.release_id == Some(deployment.id);
                            <li>
                                <a
                                    href=(format!("{here}?deployment={}", deployment.id))
                                    aria-current=(selected.then_some("page"))
                                    class="flex items-start gap-3 rounded-lg border border-transparent p-3 hover:bg-foreground/5 aria-[current=page]:border-border aria-[current=page]:bg-card"
                                >
                                    status_icon(status: deployment.status, live: live)
                                    <span class="flex min-w-0 flex-1 flex-col gap-0.5">
                                        <span class="flex items-center gap-2 text-sm">
                                            <span class="font-medium">"#" (deployment.id.to_string())</span>
                                            <span class="text-muted-foreground">(status_label(deployment.status, live))</span>
                                        </span>
                                        if let Some(commit) = &deployment.commit {
                                            <span class="truncate text-xs text-muted-foreground">
                                                <span class="font-mono">(commit.get(..7).unwrap_or(commit))</span>
                                                " "
                                                (deployment.commit_message.as_deref().unwrap_or(""))
                                            </span>
                                        }
                                        <span class="text-xs text-muted-foreground">
                                            created_at(seconds: deployment.created_at)
                                            " · "
                                            (took(deployment))
                                        </span>
                                    </span>
                                </a>
                            </li>
                        }
                    </ul>
                </nav>
                if let Some(deployment) = current {
                    <section class="flex min-w-0 flex-col gap-4 rounded-xl border border-border bg-card p-5">
                        <header class="flex flex-wrap items-center gap-x-3 gap-y-1">
                            <h2 class="font-semibold">"Deployment #" (deployment.id.to_string())</h2>
                            <span class="inline-flex items-center gap-1.5 text-sm text-muted-foreground">
                                status_icon(status: deployment.status, live: service.release_id == Some(deployment.id))
                                (status_label(deployment.status, service.release_id == Some(deployment.id)))
                            </span>
                        </header>
                        log_lines(log: &log, empty: "Nothing written yet.")
                    </section>
                }
            </div>
        }
    })
}

/// A deployment's state as an icon: spinning while in progress.
#[component]
async fn status_icon(status: DeploymentStatus, live: bool) -> Result<impl View> {
    Ok(view! {
        match status {
            DeploymentStatus::Queued => {
                icon(data: iconify_icon!("lucide:clock"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0 text-muted-foreground" })
            }
            DeploymentStatus::Building | DeploymentStatus::Starting => {
                icon(data: iconify_icon!("lucide:loader-circle"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0 animate-spin text-primary" })
            }
            DeploymentStatus::Succeeded => {
                icon(
                    data: iconify_icon!("lucide:circle-check"),
                    size: 16,
                    attrs: attributes! { class=(if live { "mt-0.5 shrink-0 text-emerald-500" } else { "mt-0.5 shrink-0 text-muted-foreground" }) }
                )
            }
            DeploymentStatus::Failed => {
                icon(data: iconify_icon!("lucide:circle-x"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0 text-destructive" })
            }
        }
    })
}

/// A deployment's state in a word. A deployment that succeeded is live until a later
/// one replaces it.
fn status_label(status: DeploymentStatus, live: bool) -> &'static str {
    match status {
        DeploymentStatus::Queued => "Queued",
        DeploymentStatus::Building => "Building",
        DeploymentStatus::Starting => "Starting",
        DeploymentStatus::Succeeded if live => "Live",
        DeploymentStatus::Succeeded => "Replaced",
        DeploymentStatus::Failed => "Failed",
    }
}

/// How long a deployment took, or has taken so far, such as `1m 05s`.
fn took(deployment: &Deployment) -> String {
    let end = deployment.finished_at.unwrap_or_else(now);
    let seconds = (end - deployment.created_at).max(0);
    match seconds {
        ..60 => format!("{seconds}s"),
        _ => format!("{}m {:02}s", seconds / 60, seconds % 60),
    }
}
