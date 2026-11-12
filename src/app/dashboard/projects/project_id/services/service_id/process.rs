//! The Advanced tab: how oxploy runs the service, and what runs after each build.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{SeeOther, bad_request, see_other},
        href, route,
    },
    view::{View, attributes, component, element_id, view},
};

use super::super::super::ProjectId;
use super::{ServiceId, ServiceTab, current_service, settings_card};
use crate::components::button::button;
use crate::components::field::{field, field_description, field_label};
use crate::components::tabs::{tabs_list, tabs_trigger};
use crate::components::textarea::textarea;
use crate::features::app_files::AppFiles;
use crate::features::deployment::Deployment;
use crate::features::docker;
use crate::features::project::Project;
use crate::features::service::{MAX_SCRIPT_LEN, Service};

/// The runtime the service runs on, and the script after its builds.
#[component]
pub async fn advanced_tab(
    cx: &Cx,
    project: &Project,
    service: &Service,
    here: &str,
) -> Result<impl View> {
    let files = AppFiles::of(&service.app_name);
    let after_build_url =
        href!(save_after_build, ProjectId(project.id), ServiceId(service.id)).resolve(cx);
    let after_build_id = element_id(cx);
    let in_docker = service.runs_in_docker();
    // The runtime follows the build, which the General tab chooses.
    let general = href!(super::page, ProjectId(project.id), ServiceId(service.id)).resolve(cx);
    let (native_href, docker_href) = match in_docker {
        true => (format!("{general}?build=cargo"), here.to_owned()),
        false => (here.to_owned(), format!("{general}?build=dockerfile")),
    };
    let image = match service.release_id {
        Some(id) if in_docker => Deployment::find_of(cx, service.id, id).await?.and_then(|release| release.image),
        _ => None,
    };
    let inner_port = service.app_port();

    Ok(view! {
        <div class="flex flex-col gap-6">
            settings_card(
                title: "Runtime",
                text: "How oxploy runs the app.",
                tabs_list(
                    tabs_trigger(
                        active: !in_docker,
                        attrs: attributes! { href=(&native_href) },
                        icon(data: iconify_icon!("lucide:cpu"), size: 14)
                        "Native"
                    )
                    tabs_trigger(
                        active: in_docker,
                        attrs: attributes! { href=(&docker_href) },
                        icon(data: iconify_icon!("lucide:container"), size: 14)
                        "Docker"
                    )
                )
                if in_docker {
                    <dl class="grid grid-cols-1 gap-x-6 gap-y-3 text-sm sm:grid-cols-[8rem_minmax(0,1fr)]">
                        <dt class="font-medium">"Container"</dt>
                        <dd class="font-mono break-all text-muted-foreground">(docker::container_name(&service.app_name))</dd>
                        <dt class="font-medium">"Image"</dt>
                        <dd class="text-muted-foreground">
                            if let Some(image) = &image {
                                <span class="font-mono break-all">(image)</span>
                            } else {
                                "Built at the first deploy"
                            }
                        </dd>
                        <dt class="font-medium">"Port"</dt>
                        <dd class="text-muted-foreground">
                            if let (Some(port), Some(inner)) = (service.port, inner_port) {
                                <span class="font-mono">"127.0.0.1:" (port.to_string())</span>
                                " → "
                                <span class="font-mono">(inner.to_string())</span>
                                " in the container"
                            } else {
                                "Chosen at the first deploy"
                            }
                        </dd>
                        <dt class="font-medium">"Network"</dt>
                        <dd class="text-muted-foreground">
                            <span class="font-mono">(docker::network(project.id))</span>
                            ", where the project's other containers reach it at "
                            <span class="font-mono break-all">
                                "http://" (&service.app_name) ":" (inner_port.map(|port| port.to_string()).unwrap_or_else(|| "PORT".to_owned()))
                            </span>
                        </dd>
                        <dt class="font-medium">"Storage"</dt>
                        <dd class="font-mono break-all text-muted-foreground">(files.storage().display().to_string())</dd>
                    </dl>
                    field_description(
                        "The app runs in a container of its release's image, which is removed when it stops. oxploy publishes its port on 127.0.0.1, where Caddy sends its traffic, and mounts the storage folder at the same path. If it exits, oxploy starts it again, waiting longer after each crash. The Build in the General tab chooses the runtime."
                    )
                } else {
                    <dl class="grid grid-cols-1 gap-x-6 gap-y-3 text-sm sm:grid-cols-[8rem_minmax(0,1fr)]">
                        <dt class="font-medium">"Port"</dt>
                        <dd class="text-muted-foreground">
                            if let Some(port) = service.port {
                                <span class="font-mono">"127.0.0.1:" (port.to_string())</span>
                            } else {
                                "Chosen at the first deploy"
                            }
                        </dd>
                        <dt class="font-medium">"Files"</dt>
                        <dd class="font-mono break-all text-muted-foreground">(files.root().display().to_string())</dd>
                    </dl>
                    field_description(
                        "The app runs as a process on this machine, in its release's code at the build path. It must listen on 127.0.0.1, at the port oxploy passes in PORT. If it exits, oxploy starts it again, waiting longer after each crash. The Build in the General tab chooses the runtime."
                    )
                }
            )
            settings_card(
                title: "After the build",
                text: "A script that runs after each build, before the new release starts, such as one that migrates the database. If it fails, the deploy stops and the running release stays.",
                <form method="post" action=(&after_build_url) class="flex flex-col gap-4">
                    field(
                        field_label(attrs: attributes! { for=(&after_build_id) }, "Script")
                        textarea(
                            attrs: attributes! {
                                id=(&after_build_id)
                                name="script"
                                class="min-h-32 font-mono leading-relaxed"
                                placeholder="my-app migrate"
                                maxlength=(MAX_SCRIPT_LEN.to_string())
                                spellcheck="false"
                                autocomplete="off"
                                autocapitalize="off"
                                wrap="off"
                            },
                            (service.after_build.as_deref().unwrap_or(""))
                        )
                        if in_docker {
                            field_description(
                                "Runs with sh -e in a container of the new image, with the app's environment variables, storage and network. The image must have sh. Leave it empty to run nothing."
                            )
                        } else {
                            field_description(
                                "Runs with sh -e where the app runs, once the databases from the Storage tab are linked, with the app's environment variables and the new binary on the PATH. Leave it empty to run nothing."
                            )
                        }
                    )
                    <div class="flex justify-end">
                        button(attrs: attributes! { type="submit" }, "Save script")
                    </div>
                </form>
            )
        </div>
    })
}

/// A form with the script that runs after each build.
#[derive(Deserialize)]
struct AfterBuildForm {
    #[serde(default)]
    script: String,
}

/// Saves the script that runs after each build.
#[route(POST "./after-build")]
async fn save_after_build(cx: &Cx, Form(form): Form<AfterBuildForm>) -> Result<SeeOther> {
    let (project, mut service) = current_service(cx).await?;
    if !service.set_after_build(cx, &form.script).await? {
        return Err(bad_request("The script is too long").into());
    }
    Ok(see_other(ServiceTab::Advanced.url(cx, &project, &service)))
}
