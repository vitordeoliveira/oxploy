//! The projects of the active organization, and the dialogs shared by the pages below
//! them.

pub mod project_id;

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{IconData, icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{SeeOther, bad_request, see_other},
        href, page, raw_path_params, route,
    },
    runtime::{Event, Signal, signal},
    view::{Child, View, attributes, component, element_id, view},
};

use super::require_organization;
use crate::components::alert_dialog::alert_dialog;
use crate::components::button::{ButtonSize, ButtonVariant, button, button_variants};
use crate::components::dialog::{
    dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title,
};
use crate::components::dropdown_menu::{
    dropdown_menu, dropdown_menu_content, dropdown_menu_item, dropdown_menu_separator,
    dropdown_menu_trigger,
};
use crate::components::field::{field, field_label};
use crate::components::floating::FloatingAlign;
use crate::components::input::input;
use crate::components::textarea::textarea;
use crate::features::project::{Details, MAX_DESCRIPTION_LEN, MAX_NAME_LEN, Project};
use crate::features::service::Service;
use project_id::ProjectId;
use project_id::services::service_id::{self, ServiceId};

/// The projects of the active organization, newest first.
#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    let organization = require_organization(cx).await?;
    let mut projects = Vec::new();
    for project in Project::in_organization(cx, organization.id).await? {
        let services = Service::count_in(cx, project.id).await?;
        projects.push((project, services));
    }
    let creating = signal(cx, || false);
    // Keyed by project, so each one gets signals of its own.
    let editing: Vec<_> = projects
        .iter()
        .map(|(project, _)| signal(&cx.keyed(project.id), || false))
        .collect();
    let deleting: Vec<_> = projects
        .iter()
        .map(|(project, _)| signal(&cx.keyed(project.id), || false))
        .collect();
    let return_to = href!(page).resolve(cx);
    let open_creating = creating.clone();

    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            <div class="flex flex-wrap items-end justify-between gap-4">
                <div class="flex flex-col gap-1">
                    <h1 class="text-2xl font-semibold tracking-tight">"Projects"</h1>
                    <p class="text-sm text-muted-foreground">
                        "Group the Rust apps you deploy together."
                    </p>
                </div>
                if !projects.is_empty() {
                    button(
                        attrs: attributes! { type="button" @click=$(|_e: Event| open_creating.set(true)) },
                        icon(data: iconify_icon!("lucide:plus"))
                        "New project"
                    )
                }
            </div>
            if projects.is_empty() {
                empty_state(
                    symbol: iconify_icon!("lucide:layers"),
                    title: "No projects yet",
                    text: "A project holds the services of one product, like its API and its workers.",
                    button(
                        attrs: attributes! { type="button" @click=$(|_e: Event| creating.set(true)) },
                        icon(data: iconify_icon!("lucide:plus"))
                        "New project"
                    )
                )
            } else {
                <ul class="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
                    #[key(project.id)]
                    for (index, (project, services)) in projects.iter().enumerate() {
                        project_card(
                            project: project,
                            services: *services,
                            editing: &editing[index],
                            deleting: &deleting[index]
                        )
                    }
                </ul>
            }
        </div>
        details_dialog(
            open: &creating,
            title: "New project",
            description: "Give the project a name. You add its services next.",
            action: href!(create).resolve(cx),
            placeholder: "Storefront",
            submit: "Create project"
        )
        #[key(project.id)]
        for (index, (project, services)) in projects.iter().enumerate() {
            details_dialog(
                open: &editing[index],
                title: "Edit project",
                description: "Change the name and description of this project.",
                action: href!(project_id::edit, ProjectId(project.id)).resolve(cx),
                details: Some((&project.name, project.description.as_deref())),
                placeholder: "Storefront",
                submit: "Save changes",
                return_to: Some(&return_to)
            )
            delete_dialog(
                open: &deleting[index],
                title: "Delete project",
                name: &project.name,
                consequence: match services {
                    0 => String::new(),
                    1 => " and its service".to_owned(),
                    count => format!(" and its {count} services"),
                },
                action: href!(project_id::remove, ProjectId(project.id)).resolve(cx)
            )
        }
    })
}

/// A project in the grid. The whole card opens it, and its menu edits or deletes it.
#[component]
async fn project_card(
    cx: &Cx,
    project: &Project,
    services: u64,
    editing: &Signal<bool>,
    deleting: &Signal<bool>,
) -> Result<impl View> {
    let editing = editing.clone();
    let deleting = deleting.clone();
    let menu = element_id(cx);
    let services = match services {
        1 => "1 service".to_owned(),
        count => format!("{count} services"),
    };

    Ok(view! {
        <li class="group/card relative flex min-h-36 flex-col gap-3 rounded-xl border border-border bg-card p-4 transition-colors hover:border-primary/50">
            <div class="flex items-start gap-3">
                <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
                    icon(data: iconify_icon!("lucide:folder-git-2"), size: 18)
                </span>
                <div class="flex min-w-0 flex-1 flex-col">
                    // The link covers the card, so the whole card opens the project.
                    <a
                        href=(href!(project_id::page, ProjectId(project.id)))
                        class="truncate font-medium after:absolute after:inset-0 after:rounded-xl"
                    >
                        (&project.name)
                    </a>
                    <span class="text-xs text-muted-foreground">(services)</span>
                </div>
                <div class="relative">
                    dropdown_menu(
                        dropdown_menu_trigger(
                            target: &menu,
                            attrs: attributes! {
                                class=(button_variants(ButtonVariant::Ghost, ButtonSize::Icon))
                                title="Project actions"
                                aria-label="Project actions"
                            },
                            icon(data: iconify_icon!("lucide:ellipsis"))
                        )
                        dropdown_menu_content(
                            id: &menu,
                            align: FloatingAlign::End,
                            dropdown_menu_item(
                                close: Some(&menu),
                                attrs: attributes! { @click=$(|_e: Event| editing.set(true)) },
                                icon(data: iconify_icon!("lucide:pencil"))
                                "Edit"
                            )
                            dropdown_menu_separator()
                            dropdown_menu_item(
                                close: Some(&menu),
                                attrs: attributes! {
                                    class="text-destructive"
                                    @click=$(|_e: Event| deleting.set(true))
                                },
                                icon(data: iconify_icon!("lucide:trash-2"))
                                "Delete"
                            )
                        )
                    )
                </div>
            </div>
            if let Some(description) = &project.description {
                <p class="line-clamp-2 text-sm text-muted-foreground">(description)</p>
            }
            <p class="mt-auto text-xs text-muted-foreground">
                "Created " created_at(seconds: project.created_at)
            </p>
        </li>
    })
}

/// The project and service the current page is about, as `(label, URL)` pairs for the
/// breadcrumb. Empty outside a project.
pub async fn trail(cx: &Cx) -> Result<Vec<(String, String)>> {
    // Pages inside a project capture its ID, and pages inside a service also its ID.
    let captures = |name| raw_path_params(cx).any(|(param, _)| param == name);
    let mut trail = Vec::new();
    if captures("service_id") {
        let (project, service) = service_id::current_service(cx).await?;
        trail.push((
            project.name,
            href!(project_id::page, ProjectId(project.id)).resolve(cx),
        ));
        trail.push((
            service.name,
            href!(service_id::page, ProjectId(project.id), ServiceId(service.id)).resolve(cx),
        ));
    } else if captures("project_id") {
        let project = project_id::current_project(cx).await?;
        trail.push((
            project.name,
            href!(project_id::page, ProjectId(project.id)).resolve(cx),
        ));
    }
    Ok(trail)
}

/// A dashed panel for a page or tab with nothing to show yet.
#[component]
async fn empty_state(
    symbol: IconData,
    title: &str,
    text: &str,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <div class="flex flex-1 flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border p-10 text-center">
            <span class="flex size-12 items-center justify-center rounded-xl bg-primary/10 text-primary">
                icon(data: symbol, size: 24)
            </span>
            <h2 class="font-medium">(title)</h2>
            <p class="max-w-sm text-sm text-muted-foreground">(text)</p>
            (child)
        </div>
    })
}

/// Whether a service is running. Nothing deploys yet, so every service shows as not
/// deployed.
#[component]
async fn deploy_status() -> Result<impl View> {
    Ok(view! {
        <span class="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border px-2 py-0.5 text-xs whitespace-nowrap text-muted-foreground">
            <span class="size-1.5 rounded-full bg-muted-foreground/60"></span>
            "Not deployed"
        </span>
    })
}

/// How long ago a moment was, such as `3 days ago`, with the exact time on hover.
#[component]
async fn created_at(seconds: i64) -> Result<impl View> {
    let moment = jiff::Timestamp::from_second(seconds).unwrap_or_default();
    let elapsed = (jiff::Timestamp::now().as_second() - seconds).max(0);
    let (count, unit) = match elapsed {
        ..60 => (0, ""),
        60..3_600 => (elapsed / 60, "minute"),
        3_600..86_400 => (elapsed / 3_600, "hour"),
        86_400..2_592_000 => (elapsed / 86_400, "day"),
        2_592_000..31_536_000 => (elapsed / 2_592_000, "month"),
        _ => (elapsed / 31_536_000, "year"),
    };
    let ago = match count {
        0 => "just now".to_owned(),
        1 => format!("1 {unit} ago"),
        count => format!("{count} {unit}s ago"),
    };

    Ok(view! {
        <time
            datetime=(moment.to_string())
            title=(moment.strftime("%Y-%m-%d %H:%M UTC").to_string())
        >
            (ago)
        </time>
    })
}

/// A dialog with the name and description of a new project or service, or of an
/// existing one when `details` holds its current name and description.
#[component]
async fn details_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    title: &str,
    description: &str,
    action: String,
    #[default] details: Option<(&str, Option<&str>)>,
    placeholder: &str,
    submit: &str,
    /// The page to go back to after saving. New projects and services open instead.
    #[default]
    return_to: Option<&str>,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let name_id = element_id(cx);
    let description_id = element_id(cx);
    let (name, current_description) = details.unwrap_or_default();

    Ok(view! {
        dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, (title))
                    dialog_description((description))
                )
                <form method="post" action=(&action) class="flex flex-col gap-4">
                    if let Some(return_to) = return_to {
                        <input type="hidden" name="return_to" value=(return_to)>
                    }
                    field(
                        field_label(attrs: attributes! { for=(&name_id) }, "Name")
                        input(
                            attrs: attributes! {
                                id=(&name_id)
                                name="name"
                                value=(name)
                                placeholder=(placeholder)
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
                            },
                            (current_description.unwrap_or(""))
                        )
                    )
                    dialog_footer(
                        button(
                            variant: ButtonVariant::Ghost,
                            attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                            "Cancel"
                        )
                        button(attrs: attributes! { type="submit" }, (submit))
                    )
                </form>
            )
        )
    })
}

/// A dialog that confirms deleting the project or service called `name`.
/// `consequence` names what goes with it, such as ` and its 2 services`.
#[component]
async fn delete_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    title: &str,
    name: &str,
    #[default] consequence: String,
    action: String,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let description_id = element_id(cx);

    Ok(view! {
        alert_dialog(
            open: $(open.get()),
            attrs: attributes! {
                aria-labelledby=(&title_id)
                aria-describedby=(&description_id)
            },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, (title))
                    dialog_description(
                        attrs: attributes! { id=(&description_id) },
                        "This deletes "
                        <strong class="font-medium text-foreground">(name)</strong>
                        (consequence)
                        ". It cannot be undone."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(&action)>
                        button(
                            variant: ButtonVariant::Destructive,
                            attrs: attributes! { type="submit" },
                            "Delete"
                        )
                    </form>
                )
            )
        )
    })
}

/// A form with the name and description of a project or service.
#[derive(Deserialize)]
struct DetailsForm {
    name: String,
    #[serde(default)]
    description: String,
    /// Where to go back to after saving, for forms that change an existing one.
    #[serde(default)]
    return_to: String,
}

impl DetailsForm {
    fn details(&self) -> Details<'_> {
        Details {
            name: &self.name,
            description: &self.description,
        }
    }
}

/// The response to details the form's own validation would have rejected.
const INVALID_DETAILS: &str = "The name is required, and the name and description must fit their limits";

/// Creates a project in the active organization and opens it.
#[route(POST)]
async fn create(cx: &Cx, Form(form): Form<DetailsForm>) -> Result<SeeOther> {
    let organization = require_organization(cx).await?;
    let Some(project) = Project::add(cx, organization.id, &form.details()).await? else {
        return Err(bad_request(INVALID_DETAILS).into());
    };
    Ok(see_other(
        href!(project_id::page, ProjectId(project.id)).resolve(cx),
    ))
}
