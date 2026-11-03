//! The General tab: deploying the service, where its code comes from and how cargo
//! builds it.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{IconData, icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        href, query_params, route,
    },
    runtime::{Event, Signal, connected, shard, signal},
    view::{View, attributes, component, element_id, view},
};

use super::super::super::ProjectId;
use super::{ServiceId, ServiceTab, current_service, settings_card};
use crate::app::dashboard::git;
use crate::app::dashboard::projects::created_at;
use crate::app::dashboard::require_organization;
use crate::components::button::{ButtonSize, ButtonVariant, button, button_variants};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::components::select::select;
use crate::components::textarea::textarea;
use crate::components::tabs::{tabs_list, tabs_trigger};
use crate::features::deploy::{AppStatus, Deployer, Refusal};
use crate::features::deployment::Deployment;
use crate::features::github::{Branches, GithubApp, Repository};
use crate::features::project::Project;
use crate::features::service::{
    BuildKind, BuildScript, CargoBuild, MAX_SCRIPT_LEN, Service, Source, cargo_command,
};

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
    let source_url = href!(save_source, ProjectId(project.id), ServiceId(service.id)).resolve(cx);

    Ok(view! {
        <div class="flex flex-col gap-6">
            deploy_card(project: project, service: service)
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
                            field_description("The folder the build runs in, such as the one with the Cargo.toml.")
                        )
                        <div class="flex justify-end md:col-span-2">
                            button(attrs: attributes! { type="submit" }, "Save source")
                        </div>
                    </form>
                }
            )
            build_card(project: project, service: service, here: here)
        </div>
    })
}

/// Which build the Build card shows: the one in `?build=`, or the one in use.
#[query_params(error = bad_request)]
struct ShownBuild {
    build: Option<String>,
}

/// How the service is built: with cargo and its options, or with a script of the
/// user's own. A tab for each, with a dot on the one in use.
#[component]
async fn build_card(cx: &Cx, project: &Project, service: &Service, here: &str) -> Result<impl View> {
    let in_use = service.builds_with();
    let in_use_slug = match in_use {
        BuildKind::Cargo => "cargo",
        BuildKind::Script => "script",
    };
    // The tabs switch in the browser. `?build=` picks the first one shown, which the
    // tabs also link to for browsers without the runtime.
    let first = match query_params::<ShownBuild>(cx)?.build.as_deref() {
        Some(slug @ ("cargo" | "script")) => slug,
        _ => in_use_slug,
    };
    let shown = signal(cx, || first.to_owned());
    let (shows_cargo, shows_script, shows_other) = (shown.clone(), shown.clone(), shown.clone());
    let (project_id, service_id) = (project.id, service.id);
    let cargo_url = href!(save_build, ProjectId(project_id), ServiceId(service_id)).resolve(cx);
    let script_url = href!(save_script, ProjectId(project_id), ServiceId(service_id)).resolve(cx);
    let rust_version_id = element_id(cx);
    let binary_id = element_id(cx);
    let features_id = element_id(cx);
    let script_id = element_id(cx);
    let script_binary_id = element_id(cx);
    let saved_features = service
        .cargo_features
        .as_deref()
        .map(|features| features.replace(',', ", "))
        .unwrap_or_default();
    // The cargo fields as typed, for the command under them.
    let typed_version = signal(cx, || service.rust_version.clone().unwrap_or_default());
    let typed_binary = signal(cx, || service.cargo_bin.clone().unwrap_or_default());
    let typed_features = signal(cx, || saved_features.clone());
    let (type_version, type_binary, type_features) =
        (typed_version.clone(), typed_binary.clone(), typed_features.clone());
    let tabs = [
        (BuildKind::Cargo, "cargo", "Cargo", iconify_icon!("lucide:package")),
        (BuildKind::Script, "script", "Script", iconify_icon!("lucide:square-terminal")),
    ];

    Ok(view! {
        settings_card(
            title: "Build",
            text: "How oxploy turns the code into a binary.",
            tabs_list(
                #[key(slug)]
                for (kind, slug, label, symbol) in tabs {
                    let active = shown.clone();
                    let choose = shown.clone();
                    tabs_trigger(
                        active: $(active.get() == slug),
                        attrs: attributes! {
                            href=(format!("{here}?build={slug}"))
                            @click=$(|e: Event| {
                                e.prevent_default();
                                choose.set(slug.to_owned())
                            })
                        },
                        icon(data: symbol, size: 14)
                        (label)
                        if kind == in_use {
                            <span class="size-1.5 rounded-full bg-primary" title="In use"></span>
                            <span class="sr-only">" (in use)"</span>
                        }
                    )
                }
            )
            <p
                class="flex items-center gap-2 text-sm text-muted-foreground"
                :hidden=$(shows_other.get() == in_use_slug)
            >
                icon(data: iconify_icon!("lucide:info"), size: 16, attrs: attributes! { class="shrink-0" })
                match in_use {
                    BuildKind::Cargo => "The service builds with cargo. Saving a script here switches it to the script.",
                    BuildKind::Script => "The service builds with its script. Saving here switches it to cargo.",
                }
            </p>
            <div :hidden=$(shows_cargo.get() != "cargo")>
                <form
                    method="post"
                    action=(&cargo_url)
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
                                @input=$(|e: Event| type_version.set(e.target.value))
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
                                @input=$(|e: Event| type_binary.set(e.target.value))
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
                                value=(&saved_features)
                                placeholder="None"
                                autocomplete="off"
                                @input=$(|e: Event| type_features.set(e.target.value))
                            }
                        )
                        field_description("Separated by commas, such as postgres, tls.")
                    )
                    <div class="flex flex-col gap-2 md:col-span-3">
                        <span class="text-sm font-medium">"Build command"</span>
                        <code class="overflow-x-auto rounded-md bg-muted px-3 py-2.5 font-mono text-sm whitespace-nowrap">
                            <span class="text-muted-foreground select-none">"$ "</span>
                            $({
                                let version = typed_version.get();
                                let binary = typed_binary.get();
                                let features = typed_features.get();
                                // `cargo_command`, in JavaScript.
                                raw!(
                                    r"cx.hydrate(((version, binary, features) =>
                                        'cargo' + (version ? ' +' + version : '') + ' build --release'
                                        + (binary ? ' --bin ' + binary : '')
                                        + (features.length ? ' --features ' + features.join(',') : ''))(
                                        ${version}.trim().toString(),
                                        ${binary}.trim().toString(),
                                        ${features}.toString().split(/[\s,]+/u).filter((feature) => feature),
                                    ))",
                                    cargo_command(&version, &binary, &features)
                                )
                            })
                        </code>
                        field_description("Topcoat apps also get their assets bundled, as topcoat asset bundle does.")
                    </div>
                    <div class="flex justify-end md:col-span-3">
                        button(attrs: attributes! { type="submit" }, "Save build")
                    </div>
                </form>
            </div>
            <div :hidden=$(shows_script.get() != "script")>
                <form method="post" action=(&script_url) class="flex flex-col gap-5">
                    field(
                        field_label(attrs: attributes! { for=(&script_id) }, "Script")
                        textarea(
                            attrs: attributes! {
                                id=(&script_id)
                                name="script"
                                class="min-h-40 font-mono leading-relaxed"
                                placeholder="cargo build --release --bin api\ncp \"$CARGO_TARGET_DIR/release/api\" \"$OXPLOY_OUTPUT_DIR/\""
                                maxlength=(MAX_SCRIPT_LEN.to_string())
                                spellcheck="false"
                                autocomplete="off"
                                autocapitalize="off"
                                wrap="off"
                                required=""
                            },
                            (service.build_script.as_deref().unwrap_or(""))
                        )
                        field_description(
                            "Runs with sh -e in the build path, so the first command that fails stops the build. It gets your environment variables, CARGO_TARGET_DIR with the app's cargo cache, and OXPLOY_OUTPUT_DIR: put the binary there, with any files it needs next to it."
                        )
                    )
                    field(
                        field_label(attrs: attributes! { for=(&script_binary_id) }, "Binary")
                        input(
                            attrs: attributes! {
                                id=(&script_binary_id)
                                name="binary"
                                value=(service.script_binary.as_deref().unwrap_or(""))
                                placeholder="api"
                                maxlength="128"
                                pattern="[A-Za-z0-9_\\-][A-Za-z0-9_.\\-]*"
                                autocomplete="off"
                                required=""
                            }
                        )
                        field_description("The file in OXPLOY_OUTPUT_DIR that oxploy starts.")
                    )
                    <div class="flex justify-end">
                        button(attrs: attributes! { type="submit" }, "Save script")
                    </div>
                </form>
            </div>
        )
    })
}

/// The buttons that deploy, stop and restart the service, with the release that runs.
#[component]
async fn deploy_card(cx: &Cx, project: &Project, service: &Service) -> Result<impl View> {
    let status = Deployer::get(cx).status(service);
    let release = match service.release_id {
        Some(id) => Deployment::find_of(cx, service.id, id).await?,
        None => None,
    };
    let has_source = service.repository.is_some() && service.branch.is_some();
    let (project_id, service_id) = (ProjectId(project.id), ServiceId(service.id));
    let deploy_url = href!(deploy, project_id, service_id).resolve(cx);
    let restart_url = href!(restart, ProjectId(project.id), ServiceId(service.id)).resolve(cx);
    let stop_url = href!(stop, ProjectId(project.id), ServiceId(service.id)).resolve(cx);
    let text = match status {
        AppStatus::NotDeployed => "Fetch the branch, build it and start it.",
        AppStatus::Deploying => "A deployment is in progress. Its log is in the Deployments tab.",
        AppStatus::Running => "Deploying again builds the branch's latest commit and swaps it in.",
        AppStatus::Restarting => "The app exited, and oxploy is starting it again. Its output is in the Logs tab.",
        AppStatus::Stopped => "The app is stopped. Start it again, or deploy the latest commit.",
    };

    Ok(view! {
        settings_card(
            title: "Deploy",
            text: text,
            <div class="flex flex-wrap items-center gap-2">
                <form method="post" action=(&deploy_url)>
                    button(
                        attrs: attributes! {
                            type="submit"
                            disabled=((!has_source || status == AppStatus::Deploying).then_some(""))
                        },
                        icon(data: iconify_icon!("lucide:rocket"))
                        "Deploy"
                    )
                </form>
                if release.is_some() {
                    <form method="post" action=(&restart_url)>
                        button(
                            variant: ButtonVariant::Outline,
                            attrs: attributes! { type="submit" },
                            if status == AppStatus::Stopped {
                                icon(data: iconify_icon!("lucide:play"))
                                "Start"
                            } else {
                                icon(data: iconify_icon!("lucide:rotate-cw"))
                                "Restart"
                            }
                        )
                    </form>
                    if status != AppStatus::Stopped {
                        <form method="post" action=(&stop_url)>
                            button(
                                variant: ButtonVariant::Outline,
                                attrs: attributes! { type="submit" class="hover:text-destructive" },
                                icon(data: iconify_icon!("lucide:square"))
                                "Stop"
                            )
                        </form>
                    }
                }
            </div>
            if !has_source {
                field_description("Save a source below to deploy.")
            }
            if let Some(release) = &release {
                <p class="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted-foreground">
                    <span class="font-medium text-foreground">"Release #" (release.id.to_string())</span>
                    if let Some(commit) = &release.commit {
                        <span class="font-mono text-xs">(commit.get(..7).unwrap_or(commit))</span>
                    }
                    if let Some(message) = &release.commit_message {
                        <span class="max-w-md truncate">(message)</span>
                    }
                    <span aria-hidden="true">"·"</span>
                    <span>"deployed " created_at(seconds: release.created_at)</span>
                    if let Some(port) = service.port {
                        <span aria-hidden="true">"·"</span>
                        <span class="font-mono text-xs">"127.0.0.1:" (port.to_string())</span>
                    }
                </p>
            }
        )
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
///
/// The page's first response shows only the saved repository, so it does not wait for
/// GitHub. The list comes when the browser connects and renders the shard again.
#[shard]
async fn repository_field(
    cx: &Cx,
    account: String,
    repository: Signal<String>,
) -> Result<impl View> {
    let app = installed_app(cx, &account).await?;
    let selected = repository.get_untracked();
    let id = element_id(cx);
    let loaded = connected(cx);
    let repositories = match &app {
        Some(app) if loaded => app.repositories().await.map_err(|error| error.to_string()),
        Some(_) if !selected.is_empty() => Ok(vec![Repository {
            full_name: selected.clone(),
            private: false,
        }]),
        _ => Ok(Vec::new()),
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
                    if !loaded {
                        field_description("Loading the repositories from GitHub…")
                    } else if repositories.is_empty() {
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
///
/// Like the repositories, the page's first response shows only the saved branch.
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
    let loaded = connected(cx);
    let saved = repository == saved_repository && !saved_branch.is_empty();
    let branches = match &app {
        Some(app) if loaded && !repository.is_empty() => Some(
            app.branches(&repository)
                .await
                .map_err(|error| error.to_string()),
        ),
        Some(_) if saved => Some(Ok(Branches {
            default: saved_branch.clone(),
            names: vec![saved_branch.clone()],
        })),
        _ => None,
    };

    Ok(view! {
        field(
            field_label(attrs: attributes! { for=(&id) }, "Branch")
            match branches {
                None => {
                    select(
                        attrs: attributes! { id=(&id) name="branch" disabled="" },
                        <option>(if loaded { "Choose a repository first" } else { "Loading…" })</option>
                    )
                }
                Some(Ok(branches)) => {
                    let chosen = if saved && branches.names.contains(&saved_branch) {
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

/// A form with a build script.
#[derive(Deserialize)]
struct ScriptForm {
    script: String,
    binary: String,
}

/// Makes the service build with the script in the form.
#[route(POST "./script")]
async fn save_script(cx: &Cx, Form(form): Form<ScriptForm>) -> Result<SeeOther> {
    let (project, mut service) = current_service(cx).await?;
    let build = BuildScript {
        script: &form.script,
        binary: &form.binary,
    };
    if !service.set_build_script(cx, &build).await? {
        return Err(bad_request(
            "Write a script, and name the binary it leaves as a plain file name",
        )
        .into());
    }
    Ok(see_other(general_url(cx, &project, &service)))
}

/// Starts deploying the service, then shows its log.
#[route(POST "./deploy")]
pub(super) async fn deploy(cx: &Cx) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    match Deployer::get(cx).deploy(&service).await? {
        // One already in progress shows the same way.
        Ok(_) | Err(Refusal::AlreadyDeploying) => Ok(see_other(
            ServiceTab::Deployments.url(cx, &project, &service),
        )),
        Err(Refusal::NoSource) => {
            Err(bad_request("Choose a repository and a branch before deploying").into())
        }
    }
}

/// Stops the service until it is started or deployed again.
#[route(POST "./stop")]
async fn stop(cx: &Cx) -> Result<SeeOther> {
    let (project, mut service) = current_service(cx).await?;
    Deployer::get(cx).stop(&mut service).await?;
    Ok(see_other(general_url(cx, &project, &service)))
}

/// Starts the service's release again, whether it was stopped or running.
#[route(POST "./restart")]
async fn restart(cx: &Cx) -> Result<SeeOther> {
    let (project, mut service) = current_service(cx).await?;
    Deployer::get(cx).restart(&mut service).await??;
    Ok(see_other(general_url(cx, &project, &service)))
}

/// The service's page, which opens on this tab.
fn general_url(cx: &Cx, project: &Project, service: &Service) -> String {
    href!(super::page, ProjectId(project.id), ServiceId(service.id)).resolve(cx)
}
