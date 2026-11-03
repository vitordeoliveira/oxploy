//! One project and its services.

pub mod services;

use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        href, module_param, page, path_param, route,
    },
    runtime::{Event, signal},
    view::{View, attributes, component, view},
};

use crate::app::dashboard::projects::{
    DetailsForm, INVALID_DETAILS, created_at, delete_dialog, deploy_status, details_dialog,
    empty_state,
};
use crate::app::dashboard::{back, require_organization};
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::features::deploy::Deployer;
use crate::features::project::Project;
use crate::features::service::Service;
use services::service_id::{self, ServiceId};

module_param!(pub project_id: u64, error = not_found);

/// Returns the project in the URL, or a not found error unless it belongs to the
/// organization the browser is working in.
pub async fn current_project(cx: &Cx) -> Result<Project> {
    let organization = require_organization(cx).await?;
    let id = *path_param::<ProjectId>(cx)?;
    let project = Project::find_in(cx, organization.id, id)
        .await?
        .ok_or_not_found()?;
    Ok(project)
}

/// The project's services, oldest first.
#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    let project = current_project(cx).await?;
    let services = Service::in_project(cx, project.id).await?;
    let editing = signal(cx, || false);
    let deleting = signal(cx, || false);
    let creating = signal(cx, || false);
    let open_editing = editing.clone();
    let open_deleting = deleting.clone();
    let open_creating = creating.clone();
    let return_to = href!(page, ProjectId(project.id)).resolve(cx);

    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            <div class="flex flex-wrap items-start justify-between gap-4">
                <div class="flex min-w-0 flex-col gap-1">
                    <h1 class="truncate text-2xl font-semibold tracking-tight">(&project.name)</h1>
                    <p class="text-sm text-muted-foreground">
                        (project.description.as_deref().unwrap_or("The services this project deploys."))
                    </p>
                </div>
                <div class="flex items-center gap-2">
                    button(
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Icon,
                        attrs: attributes! {
                            type="button"
                            title="Edit project"
                            aria-label="Edit project"
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
                            title="Delete project"
                            aria-label="Delete project"
                            @click=$(|_e: Event| open_deleting.set(true))
                        },
                        icon(data: iconify_icon!("lucide:trash-2"))
                    )
                    if !services.is_empty() {
                        button(
                            attrs: attributes! { type="button" @click=$(|_e: Event| open_creating.set(true)) },
                            icon(data: iconify_icon!("lucide:plus"))
                            "New service"
                        )
                    }
                </div>
            </div>
            if services.is_empty() {
                empty_state(
                    symbol: iconify_icon!("lucide:box"),
                    title: "No services yet",
                    text: "A service is one Rust app: a repository that oxploy builds with cargo and keeps running.",
                    button(
                        attrs: attributes! { type="button" @click=$(|_e: Event| creating.set(true)) },
                        icon(data: iconify_icon!("lucide:plus"))
                        "New service"
                    )
                )
            } else {
                <ul class="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
                    #[key(service.id)]
                    for service in &services {
                        service_card(project: &project, service: service)
                    }
                </ul>
            }
        </div>
        details_dialog(
            open: &editing,
            title: "Edit project",
            description: "Change the name and description of this project.",
            action: href!(edit, ProjectId(project.id)).resolve(cx),
            details: Some((&project.name, project.description.as_deref())),
            placeholder: "Storefront",
            submit: "Save changes",
            return_to: Some(&return_to)
        )
        delete_dialog(
            open: &deleting,
            title: "Delete project",
            name: &project.name,
            consequence: match services.len() {
                0 => String::new(),
                1 => " and its service".to_owned(),
                count => format!(" and its {count} services"),
            },
            action: href!(remove, ProjectId(project.id)).resolve(cx)
        )
        details_dialog(
            open: &creating,
            title: "New service",
            description: "A Rust app in this project. You connect its repository after creating it.",
            action: href!(services::create, ProjectId(project.id)).resolve(cx),
            placeholder: "API",
            submit: "Create service"
        )
    })
}

/// A service in the grid. The whole card opens it.
#[component]
async fn service_card(cx: &Cx, project: &Project, service: &Service) -> Result<impl View> {
    let status = Deployer::get(cx).status(service);

    Ok(view! {
        <li class="relative flex min-h-36 flex-col gap-3 rounded-xl border border-border bg-card p-4 transition-colors hover:border-primary/50">
            <div class="flex items-start gap-3">
                <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
                    icon(data: iconify_icon!("lucide:box"), size: 18)
                </span>
                <div class="flex min-w-0 flex-1 flex-col">
                    // The link covers the card, so the whole card opens the service.
                    <a
                        href=(href!(service_id::page, ProjectId(project.id), ServiceId(service.id)))
                        class="truncate font-medium after:absolute after:inset-0 after:rounded-xl"
                    >
                        (&service.name)
                    </a>
                    <span class="truncate font-mono text-xs text-muted-foreground">(&service.app_name)</span>
                </div>
                deploy_status(status: status)
            </div>
            if let Some(description) = &service.description {
                <p class="line-clamp-2 text-sm text-muted-foreground">(description)</p>
            }
            <p class="mt-auto text-xs text-muted-foreground">
                "Created " created_at(seconds: service.created_at)
            </p>
        </li>
    })
}

/// Changes the name and description of the project.
#[route(POST "./edit")]
pub async fn edit(cx: &Cx, Form(form): Form<DetailsForm>) -> Result<SeeOther> {
    let mut project = current_project(cx).await?;
    if !project.edit(cx, &form.details()).await? {
        return Err(bad_request(INVALID_DETAILS).into());
    }
    Ok(back(cx, &form.return_to))
}

/// Deletes the project with its services, then shows the remaining projects.
#[route(POST "./delete")]
pub async fn remove(cx: &Cx) -> Result<SeeOther> {
    let project = current_project(cx).await?;
    project.remove(cx).await?;
    Ok(see_other(href!(super::page).resolve(cx)))
}
