//! The General tab: where the service's code comes from and how cargo builds it.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{IconData, icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        href, route,
    },
    runtime::{Event, Signal, shard, signal},
    view::{View, attributes, component, element_id, view},
};

use super::super::super::ProjectId;
use super::{ServiceId, current_service, settings_card};
use crate::app::dashboard::git;
use crate::app::dashboard::require_organization;
use crate::components::button::{ButtonSize, ButtonVariant, button, button_variants};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::components::select::select;
use crate::components::tabs::{tabs_list, tabs_trigger};
use crate::features::github::GithubApp;
use crate::features::project::Project;
use crate::features::service::{CargoBuild, Service, Source};

/// The service's source and build settings.
#[component]
pub async fn general_tab(
    cx: &Cx,
    project: &Project,
    service: &Service,
    here: &str,
) -> Result<impl View> {
    let apps: Vec<_> = GithubApp::in_organization(cx, project.organization_id)
        .await?
        .into_iter()
        .filter(|app| app.installation_id.is_some())
        .collect();
    // The saved account while it is still connected, or else the first one.
    let saved_app = service
        .github_app_id
        .filter(|id| apps.iter().any(|app| app.id == *id));
    let chosen_app = saved_app.or_else(|| apps.first().map(|app| app.id));
    let account = signal(cx, || chosen_app.map(|id| id.to_string()).unwrap_or_default());
    let repository = signal(cx, || match saved_app {
        Some(_) => service.repository.clone().unwrap_or_default(),
        None => String::new(),
    });
    let choose_account = account.clone();
    let clear_repository = repository.clone();
    let account_id = element_id(cx);
    let build_path_id = element_id(cx);
    let rust_version_id = element_id(cx);
    let binary_id = element_id(cx);
    let features_id = element_id(cx);
    let source_url = href!(save_source, ProjectId(project.id), ServiceId(service.id)).resolve(cx);
    let build_url = href!(save_build, ProjectId(project.id), ServiceId(service.id)).resolve(cx);

    Ok(view! {
        <div class="flex flex-col gap-6">
            settings_card(
                title: "Source",
                text: "Where oxploy gets the code to build.",
                tabs_list(
                    tabs_trigger(
                        active: true,
                        attrs: attributes! { href=(here) },
                        icon(data: iconify_icon!("lucide:github"), size: 14)
                        "GitHub"
                    )
                )
                if apps.is_empty() {
                    <div class="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-dashed border-border p-4">
                        <p class="text-sm text-muted-foreground">
                            "Connect a GitHub account to choose a repository."
                        </p>
                        <a href=(href!(git::page)) class=(button_variants(ButtonVariant::Primary, ButtonSize::Sm))>
                            icon(data: iconify_icon!("lucide:github"))
                            "Connect GitHub"
                        </a>
                    </div>
                } else {
                    <form
                        method="post"
                        action=(&source_url)
                        class="grid grid-cols-1 gap-5 md:grid-cols-2"
                    >
                        field(
                            field_label(attrs: attributes! { for=(&account_id) }, "GitHub account")
                            select(
                                attrs: attributes! {
                                    id=(&account_id)
                                    name="github_app_id"
                                    required=""
                                    @change=$(|e: Event| {
                                        choose_account.set(e.target.value);
                                        clear_repository.set("".to_owned())
                                    })
                                },
                                #[key(app.id)]
                                for app in &apps {
                                    <option
                                        value=(app.id.to_string())
                                        selected=((Some(app.id) == chosen_app).then_some(""))
                                    >
                                        (app.account.as_deref().unwrap_or(&app.name))
                                    </option>
                                }
                            )
                        )
                        repository_field(account: $(account.get()), repository: repository.clone())
                        branch_field(
                            account: $(account.get()),
                            repository: $(repository.get()),
                            saved_repository: service.repository.clone().unwrap_or_default(),
                            saved_branch: service.branch.clone().unwrap_or_default()
                        )
                        field(
                            field_label(attrs: attributes! { for=(&build_path_id) }, "Build path")
                            input(
                                attrs: attributes! {
                                    id=(&build_path_id)
                                    name="build_path"
                                    value=(service.build_path.as_deref().map(|path| format!("/{path}")).unwrap_or_default())
                                    placeholder="/"
                                    maxlength="255"
                                    autocomplete="off"
                                }
                            )
                            field_description("The folder with the Cargo.toml to build.")
                        )
                        <div class="flex justify-end md:col-span-2">
                            button(attrs: attributes! { type="submit" }, "Save source")
                        </div>
                    </form>
                }
            )
            settings_card(
                title: "Build",
                text: "How oxploy turns the code into a binary.",
                tabs_list(
                    tabs_trigger(
                        active: true,
                        attrs: attributes! { href=(here) },
                        icon(data: iconify_icon!("lucide:package"), size: 14)
                        "Cargo"
                    )
                )
                <form
                    method="post"
                    action=(&build_url)
                    class="grid grid-cols-1 gap-5 md:grid-cols-3"
                >
                    field(
                        field_label(attrs: attributes! { for=(&rust_version_id) }, "Rust version")
                        input(
                            attrs: attributes! {
                                id=(&rust_version_id)
                                name="rust_version"
                                value=(service.rust_version.as_deref().unwrap_or(""))
                                placeholder="From rust-toolchain.toml"
                                maxlength="32"
                                autocomplete="off"
                            }
                        )
                        field_description("A channel such as stable, or a release such as 1.85. Empty uses the repository's toolchain file, or stable.")
                    )
                    field(
                        field_label(attrs: attributes! { for=(&binary_id) }, "Binary")
                        input(
                            attrs: attributes! {
                                id=(&binary_id)
                                name="binary"
                                value=(service.cargo_bin.as_deref().unwrap_or(""))
                                placeholder="The package's only binary"
                                maxlength="64"
                                autocomplete="off"
                            }
                        )
                        field_description("Which binary to build, for packages with more than one.")
                    )
                    field(
                        field_label(attrs: attributes! { for=(&features_id) }, "Features")
                        input(
                            attrs: attributes! {
                                id=(&features_id)
                                name="features"
                                value=(service.cargo_features.as_deref().map(|features| features.replace(',', ", ")).unwrap_or_default())
                                placeholder="None"
                                autocomplete="off"
                            }
                        )
                        field_description("Separated by commas, such as postgres, tls.")
                    )
                    <div class="flex flex-col gap-2 md:col-span-3">
                        <span class="text-sm font-medium">"Build command"</span>
                        <code class="overflow-x-auto rounded-md bg-muted px-3 py-2.5 font-mono text-sm whitespace-nowrap">
                            <span class="text-muted-foreground select-none">"$ "</span>
                            (service.build_command())
                        </code>
                    </div>
                    <div class="flex justify-end md:col-span-3">
                        button(attrs: attributes! { type="submit" }, "Save build")
                    </div>
                </form>
            )
        </div>
    })
}

/// The installed GitHub App with the ID in `account`, if it belongs to the
/// organization the browser is working in.
///
/// Shards answer requests of their own, without the page's guards, so they check
/// access here.
async fn installed_app(cx: &Cx, account: &str) -> Result<Option<GithubApp>> {
    let organization = require_organization(cx).await?;
    let Ok(id) = account.parse() else {
        return Ok(None);
    };
    let app = GithubApp::find_in(cx, organization.id, id).await?;
    Ok(app.filter(|app| app.installation_id.is_some()))
}

/// The repositories of the chosen account. Choosing one updates `repository`, which
/// reloads the branches.
#[shard]
async fn repository_field(
    cx: &Cx,
    account: String,
    repository: Signal<String>,
) -> Result<impl View> {
    let app = installed_app(cx, &account).await?;
    let selected = repository.get_untracked();
    let id = element_id(cx);
    let repositories = match &app {
        Some(app) => app.repositories().await.map_err(|error| error.to_string()),
        None => Ok(Vec::new()),
    };

    Ok(view! {
        field(
            field_label(attrs: attributes! { for=(&id) }, "Repository")
            match repositories {
                Ok(repositories) => {
                    select(
                        attrs: attributes! {
                            id=(&id)
                            name="repository"
                            required=""
                            @change=$(|e: Event| repository.set(e.target.value))
                        },
                        // With `required`, this empty first option is the placeholder.
                        <option value="" selected=(selected.is_empty().then_some(""))>
                            "Choose a repository"
                        </option>
                        #[key(choice.full_name.clone())]
                        for choice in &repositories {
                            <option
                                value=(&choice.full_name)
                                selected=((choice.full_name == selected).then_some(""))
                            >
                                (&choice.full_name)
                                if choice.private { " · private" }
                            </option>
                        }
                    )
                    if repositories.is_empty() {
                        field_description(
                            "The app cannot read any repository yet. Add some in its installation settings on GitHub."
                        )
                    }
                }
                Err(problem) => {
                    problem_text(symbol: iconify_icon!("lucide:triangle-alert"), text: &problem)
                }
            }
        )
    })
}

/// The branches of the chosen repository, with the saved branch chosen when the
/// repository is the saved one, or else the repository's default branch.
#[shard]
async fn branch_field(
    cx: &Cx,
    account: String,
    repository: String,
    saved_repository: String,
    saved_branch: String,
) -> Result<impl View> {
    let app = installed_app(cx, &account).await?;
    let id = element_id(cx);
    let branches = match &app {
        Some(app) if !repository.is_empty() => Some(
            app.branches(&repository)
                .await
                .map_err(|error| error.to_string()),
        ),
        _ => None,
    };

    Ok(view! {
        field(
            field_label(attrs: attributes! { for=(&id) }, "Branch")
            match branches {
                None => {
                    select(
                        attrs: attributes! { id=(&id) name="branch" disabled="" },
                        <option>"Choose a repository first"</option>
                    )
                }
                Some(Ok(branches)) => {
                    let chosen = if repository == saved_repository && branches.names.contains(&saved_branch) {
                        saved_branch.clone()
                    } else {
                        branches.default.clone()
                    };
                    select(
                        attrs: attributes! { id=(&id) name="branch" required="" },
                        #[key(name.clone())]
                        for name in &branches.names {
                            <option value=(name) selected=((*name == chosen).then_some(""))>(name)</option>
                        }
                    )
                }
                Some(Err(problem)) => {
                    problem_text(symbol: iconify_icon!("lucide:triangle-alert"), text: &problem)
                }
            }
        )
    })
}

/// A short message about something that went wrong, in place of a field's control.
#[component]
async fn problem_text(symbol: IconData, text: &str) -> Result<impl View> {
    Ok(view! {
        <p class="flex min-h-10 items-center gap-2 text-sm text-destructive">
            icon(data: symbol, size: 16, attrs: attributes! { class="shrink-0" })
            (text)
        </p>
    })
}

/// A form with where the service's code comes from.
#[derive(Deserialize)]
struct SourceForm {
    github_app_id: u64,
    repository: String,
    branch: String,
    #[serde(default)]
    build_path: String,
}

/// Saves where the service's code comes from.
#[route(POST "./source")]
async fn save_source(cx: &Cx, Form(form): Form<SourceForm>) -> Result<SeeOther> {
    let (project, mut service) = current_service(cx).await?;
    // The account must be one of the service's organization.
    GithubApp::find_in(cx, project.organization_id, form.github_app_id)
        .await?
        .ok_or_not_found()?;
    let source = Source {
        github_app_id: form.github_app_id,
        repository: &form.repository,
        branch: &form.branch,
        build_path: &form.build_path,
    };
    if !service.set_source(cx, &source).await? {
        return Err(bad_request(
            "Choose a repository and a branch, and a build path inside the repository",
        )
        .into());
    }
    Ok(see_other(general_url(cx, &project, &service)))
}

/// A form with how cargo builds the service.
#[derive(Deserialize)]
struct BuildForm {
    #[serde(default)]
    rust_version: String,
    #[serde(default)]
    binary: String,
    #[serde(default)]
    features: String,
}

/// Saves how cargo builds the service.
#[route(POST "./build")]
async fn save_build(cx: &Cx, Form(form): Form<BuildForm>) -> Result<SeeOther> {
    let (project, mut service) = current_service(cx).await?;
    let build = CargoBuild {
        rust_version: &form.rust_version,
        binary: &form.binary,
        features: &form.features,
    };
    if !service.set_build(cx, &build).await? {
        return Err(bad_request(
            "The Rust version, binary or features are not ones cargo accepts",
        )
        .into());
    }
    Ok(see_other(general_url(cx, &project, &service)))
}

/// The service's page, which opens on this tab.
fn general_url(cx: &Cx, project: &Project, service: &Service) -> String {
    href!(super::page, ProjectId(project.id), ServiceId(service.id)).resolve(cx)
}
