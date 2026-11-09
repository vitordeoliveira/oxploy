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
    runtime::{Event, Signal, signal},
    view::{View, attributes, component, element_id, view},
};

use crate::app::dashboard::projects::{
    DetailsForm, INVALID_DETAILS, created_at, delete_dialog, deploy_status, details_dialog,
    empty_state,
};
use crate::app::dashboard::{back, require_organization};
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::dialog::{dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::components::textarea::textarea;
use crate::features::app_mcp::{MAX_TOKEN_LEN, MAX_URL_LEN};
use crate::features::project::{MAX_DESCRIPTION_LEN, MAX_NAME_LEN};
use crate::features::deploy::Deployer;
use crate::features::project::Project;
use crate::features::service::Service;
use services::service_id::{self, ServiceId, external_badge};

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
                    text: "A service is one Rust app, a repository that oxploy builds with cargo and keeps running, or an MCP server that runs elsewhere.",
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
        new_service_dialog(open: &creating, action: href!(services::create, ProjectId(project.id)).resolve(cx))
    })
}

/// How a choice of the kind of a new service looks, marked while it is chosen.
const KIND_CHOICE: &str = "flex cursor-pointer flex-col gap-1 rounded-lg border border-border p-3 text-sm transition-colors has-[:checked]:border-primary has-[:checked]:bg-primary/5";

/// A dialog that makes a service: a Rust app, whose repository is connected after, or an
/// external MCP server, with the URL it answers at and the token it takes.
#[component]
async fn new_service_dialog(cx: &Cx, open: &Signal<bool>, action: String) -> Result<impl View> {
    let open = open.clone();
    let external = signal(cx, || false);
    let (to_app, to_external) = (external.clone(), external.clone());
    let title_id = element_id(cx);
    let name_id = element_id(cx);
    let description_id = element_id(cx);
    let url_id = element_id(cx);
    let token_id = element_id(cx);

    Ok(view! {
        dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "New service")
                    dialog_description("A Rust app this project deploys, or an MCP server that runs elsewhere, whose tools reach this organization's MCP.")
                )
                <form method="post" action=(&action) class="flex flex-col gap-4">
                    <fieldset class="grid grid-cols-1 gap-2 sm:grid-cols-2">
                        <legend class="sr-only">"Kind"</legend>
                        <label class=(KIND_CHOICE)>
                            <span class="flex items-center gap-2 font-medium">
                                <input
                                    type="radio"
                                    name="kind"
                                    value="app"
                                    checked=""
                                    class="accent-primary"
                                    @change=$(|_e: Event| to_app.set(false))
                                >
                                icon(data: iconify_icon!("lucide:box"), size: 16)
                                "Rust app"
                            </span>
                            <span class="text-xs text-muted-foreground">"A repository oxploy builds with cargo and keeps running. You connect it after."</span>
                        </label>
                        <label class=(KIND_CHOICE)>
                            <span class="flex items-center gap-2 font-medium">
                                <input
                                    type="radio"
                                    name="kind"
                                    value="external_mcp"
                                    class="accent-primary"
                                    @change=$(|_e: Event| to_external.set(true))
                                >
                                icon(data: iconify_icon!("lucide:plug"), size: 16)
                                "External MCP server"
                            </span>
                            <span class="text-xs text-muted-foreground">"An MCP server that runs elsewhere, which oxploy calls at its URL."</span>
                        </label>
                    </fieldset>
                    field(
                        field_label(attrs: attributes! { for=(&name_id) }, "Name")
                        input(
                            attrs: attributes! {
                                id=(&name_id)
                                name="name"
                                placeholder="API"
                                maxlength=(MAX_NAME_LEN.to_string())
                                required=""
                            }
                        )
                    )
                    field(
                        field_label(attrs: attributes! { for=(&description_id) }, "Description")
                        textarea(
                            attrs: attributes! {
                                id=(&description_id)
                                name="description"
                                placeholder="Optional"
                                maxlength=(MAX_DESCRIPTION_LEN.to_string())
                            }
                        )
                    )
                    <div class="flex flex-col gap-4" :hidden=$(!external.get())>
                        field(
                            field_label(attrs: attributes! { for=(&url_id) }, "URL")
                            input(
                                attrs: attributes! {
                                    id=(&url_id)
                                    name="url"
                                    type="url"
                                    placeholder="https://api.example.com/mcp"
                                    maxlength=(MAX_URL_LEN.to_string())
                                    pattern="https://.+"
                                    title="An https:// URL, such as https://api.example.com/mcp"
                                    :required=$(external.get())
                                    class="font-mono"
                                    spellcheck="false"
                                    autocomplete="off"
                                    autocapitalize="off"
                                }
                            )
                            field_description("Where the server answers MCP over Streamable HTTP. oxploy calls it over HTTPS only.")
                        )
                        field(
                            field_label(attrs: attributes! { for=(&token_id) }, "Token")
                            input(
                                attrs: attributes! {
                                    id=(&token_id)
                                    name="token"
                                    placeholder="Optional"
                                    maxlength=(MAX_TOKEN_LEN.to_string())
                                    class="font-mono"
                                    spellcheck="false"
                                    autocomplete="off"
                                    autocapitalize="off"
                                }
                            )
                            field_description("The token the server takes as Authorization: Bearer <token>. Leave it empty and oxploy makes one, for you to put in the server's configuration.")
                        )
                    </div>
                    dialog_footer(
                        button(
                            variant: ButtonVariant::Ghost,
                            attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                            "Cancel"
                        )
                        button(attrs: attributes! { type="submit" }, "Create service")
                    )
                </form>
            )
        )
    })
}

/// A service in the grid. The whole card opens it.
#[component]
async fn service_card(cx: &Cx, project: &Project, service: &Service) -> Result<impl View> {
    let status = Deployer::get(cx).status(service);
    let external = service.is_external();

    Ok(view! {
        <li class="relative flex min-h-36 flex-col gap-3 rounded-xl border border-border bg-card p-4 transition-colors hover:border-primary/50">
            <div class="flex items-start gap-3">
                <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
                    if external {
                        icon(data: iconify_icon!("lucide:plug"), size: 18)
                    } else {
                        icon(data: iconify_icon!("lucide:box"), size: 18)
                    }
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
                if external {
                    external_badge()
                } else {
                    deploy_status(status: status)
                }
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
