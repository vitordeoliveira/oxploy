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
    runtime::{Event, Signal, connected, procedure, shard, signal},
    view::{View, attributes, component, element_id, view},
};

use super::super::super::ProjectId;
use super::hosts::option_switch;
use super::{ServiceId, ServiceTab, current_service, docker_unavailable, settings_card};
use crate::app::dashboard::git;
use crate::app::dashboard::projects::created_at;
use crate::app::dashboard::require_organization;
use crate::components::button::{ButtonSize, ButtonVariant, button, button_variants};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::components::select::select;
use crate::components::textarea::textarea;
use crate::components::tabs::{tabs_list, tabs_trigger};
use crate::features::archive::{self, Runnable};
use crate::features::deploy::{AppStatus, Deployer, Refusal};
use crate::features::docker;
use crate::features::deployment::Deployment;
use crate::features::github::{Branches, GithubApp, Release, Repository};
use crate::features::project::Project;
use crate::features::machine::{Machine, named_for_this_machine};
use crate::features::time_zone;
use crate::features::service::{
    BuildKind, BuildScript, CargoBuild, DockerfileBuild, MAX_SCRIPT_LEN, ReleaseBuild, Service, Source,
    cargo_command, guess_binary, release_file_matches, release_file_pattern,
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
    // How many times the deploy card's buttons restarted, started or stopped the app.
    let changes = signal(cx, || 0usize);

    Ok(view! {
        <div class="flex flex-col gap-6">
            deploy_card(project_id: project.id, service_id: service.id, changes: changes)
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
                            field_description("The folder the build runs in, such as the one with the Cargo.toml or the Dockerfile.")
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

/// How the service is built: with cargo and its options, with a script of the user's
/// own, elsewhere, as a file of a GitHub release, or with Docker, to run in a container.
/// A tab for each, with a dot on the one in use.
#[component]
async fn build_card(cx: &Cx, project: &Project, service: &Service, here: &str) -> Result<impl View> {
    let in_use = service.builds_with();
    let in_use_slug = match in_use {
        BuildKind::Cargo => "cargo",
        BuildKind::Script => "script",
        BuildKind::Release => "release",
        BuildKind::Dockerfile => "dockerfile",
    };
    // The tabs switch in the browser. `?build=` picks the first one shown, which the
    // tabs also link to for browsers without the runtime.
    let first = match query_params::<ShownBuild>(cx)?.build.as_deref() {
        Some(slug @ ("cargo" | "script" | "release" | "dockerfile")) => slug,
        _ => in_use_slug,
    };
    let shown = signal(cx, || first.to_owned());
    let (shows_cargo, shows_script, shows_release, shows_dockerfile, shows_other) =
        (shown.clone(), shown.clone(), shown.clone(), shown.clone(), shown.clone());
    let docker_status = docker::status().await;
    let (project_id, service_id) = (project.id, service.id);
    let cargo_url = href!(save_build, ProjectId(project_id), ServiceId(service_id)).resolve(cx);
    let script_url = href!(save_script, ProjectId(project_id), ServiceId(service_id)).resolve(cx);
    let release_url = href!(save_release, ProjectId(project_id), ServiceId(service_id)).resolve(cx);
    let dockerfile_url = href!(save_dockerfile, ProjectId(project_id), ServiceId(service_id)).resolve(cx);
    let dockerfile_id = element_id(cx);
    let container_port_id = element_id(cx);
    let port_placeholder = match service.port {
        Some(port) => format!("{port}, as the PORT oxploy passes"),
        None => "The PORT oxploy passes".to_owned(),
    };
    // The release's file as chosen, which the binaries to choose from come from.
    let release_file = signal(cx, || service.github_asset.clone().unwrap_or_default());
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
        (BuildKind::Release, "release", "GitHub release", iconify_icon!("lucide:tag")),
        (BuildKind::Dockerfile, "dockerfile", "Dockerfile", iconify_icon!("lucide:container")),
    ];

    Ok(view! {
        settings_card(
            title: "Build",
            text: "How oxploy turns the code into a binary or an image, or where it gets a binary built elsewhere.",
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
                    BuildKind::Cargo => "The service builds with cargo. Saving this tab switches it.",
                    BuildKind::Script => "The service builds with its script. Saving this tab switches it.",
                    BuildKind::Release => "The service deploys its GitHub releases. Saving this tab switches it.",
                    BuildKind::Dockerfile => "The service builds with Docker and runs in a container. Saving this tab switches it.",
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
            <div :hidden=$(shows_release.get() != "release")>
                <form method="post" action=(&release_url) class="flex flex-col gap-5">
                    <p class="text-sm text-muted-foreground">
                        "Deploying downloads a file of the repository's latest release, built elsewhere, such as in GitHub Actions, for this server: "
                        (Machine::this().to_string())
                        ". The branch and build path above do not apply."
                    </p>
                    <div class="grid grid-cols-1 gap-5 md:grid-cols-2">
                        release_file_field(project_id: project_id, service_id: service_id, file: release_file.clone())
                        release_binary_field(project_id: project_id, service_id: service_id, file: $(release_file.get()))
                    </div>
                    option_switch(
                        name: "automatic",
                        checked: service.deploys_new_releases == Some(true),
                        label: "Deploy new releases",
                        text: "oxploy looks at the repository every minute and deploys a new release once its file is there. A release that fails to deploy is not tried again on its own."
                    )
                    <div class="flex justify-end">
                        button(attrs: attributes! { type="submit" }, "Save release")
                    </div>
                </form>
            </div>
            <div :hidden=$(shows_dockerfile.get() != "dockerfile")>
                <form method="post" action=(&dockerfile_url) class="flex flex-col gap-5">
                    if let Err(why) = &docker_status {
                        docker_unavailable(why: why)
                    }
                    <p class="text-sm text-muted-foreground">
                        "Deploying runs docker build in the build path, and runs the image in a container on the project's network, where the project's other containers reach it by its app name, "
                        <code class="font-mono">(&service.app_name)</code>
                        ". The build gets none of your variables; the container gets them when it starts."
                    </p>
                    <div class="grid grid-cols-1 gap-5 md:grid-cols-2">
                        field(
                            field_label(attrs: attributes! { for=(&dockerfile_id) }, "Dockerfile")
                            input(
                                attrs: attributes! {
                                    id=(&dockerfile_id)
                                    name="dockerfile"
                                    value=(service.dockerfile.as_deref().unwrap_or(""))
                                    placeholder="Dockerfile"
                                    maxlength="255"
                                    autocomplete="off"
                                    spellcheck="false"
                                }
                            )
                            field_description("From the build path, such as deploy/Dockerfile.")
                        )
                        field(
                            field_label(attrs: attributes! { for=(&container_port_id) }, "Container port")
                            input(
                                attrs: attributes! {
                                    id=(&container_port_id)
                                    name="port"
                                    type="number"
                                    min="1"
                                    max="65535"
                                    value=(service.container_port.map(|port| port.to_string()).unwrap_or_default())
                                    placeholder=(&port_placeholder)
                                    autocomplete="off"
                                }
                            )
                            field_description("The port the app listens on in its container, which it gets in PORT, with HOST=0.0.0.0. oxploy publishes it on 127.0.0.1 at the app's port.")
                        )
                    </div>
                    <div class="flex justify-end">
                        button(
                            attrs: attributes! { type="submit" disabled=(docker_status.is_err().then_some("")) },
                            "Save Dockerfile"
                        )
                    </div>
                </form>
            </div>
        )
    })
}

/// The buttons that deploy, stop and restart the service, with the release that runs.
///
/// Restarting, starting and stopping happen without a reload: while one runs, the
/// buttons wait and the card says so, then it says what became of it, with the time,
/// and renders again, as `changes` counts each one. Deploying opens the deployment's
/// log, and says it is on its way until then. A shard answers requests of its own, so
/// it checks access itself.
#[shard]
pub(super) async fn deploy_card(cx: &Cx, project_id: u64, service_id: u64, changes: Signal<usize>) -> Result<impl View> {
    let (project, service) = find_service(cx, project_id, service_id).await?;
    // Read so that each change renders the card again.
    let _ = changes.get();
    let status = Deployer::get(cx).status(&service);
    let release = match service.release_id {
        Some(id) => Deployment::find_of(cx, service.id, id).await?,
        None => None,
    };
    let has_source = service.repository.is_some() && service.branch.is_some();
    let can_deploy = has_source && status != AppStatus::Deploying;
    let deploy_url = href!(deploy, ProjectId(project.id), ServiceId(service.id)).resolve(cx);
    let releases = service.builds_with() == BuildKind::Release;
    let stopped = status == AppStatus::Stopped;
    let text = match (status, releases) {
        (AppStatus::NotDeployed, false) => "Fetch the branch, build it and start it.",
        (AppStatus::NotDeployed, true) => "Download the repository's latest release and start it.",
        (AppStatus::Deploying, _) => "A deployment is in progress. Its log is in the Deployments tab.",
        (AppStatus::Running, false) => "Deploying again builds the branch's latest commit and swaps it in.",
        (AppStatus::Running, true) => "Deploying again downloads the latest release and swaps it in.",
        (AppStatus::Restarting, _) => "The app exited, and oxploy is starting it again. Its output is in the Logs tab.",
        (AppStatus::Stopped, false) => "The app is stopped. Start it again, or deploy the latest commit.",
        (AppStatus::Stopped, true) => "The app is stopped. Start it again, or deploy the latest release.",
    };
    // What the last button did, or does now, said under the buttons.
    let busy = signal(cx, || false);
    let failed = signal(cx, || false);
    let note = signal(cx, String::new);
    let (deploying, restarting, stopping) = (busy.clone(), busy.clone(), busy.clone());
    let (deploy_note, restart_note, stop_note) = (note.clone(), note.clone(), note.clone());
    let (restart_failed, stop_failed) = (failed.clone(), failed.clone());
    let (restart_done, stop_done) = (changes.clone(), changes.clone());
    let restart_doing = if stopped { "Starting…" } else { "Restarting…" };

    Ok(view! {
        settings_card(
            title: "Deploy",
            text: text,
            <div class="flex flex-wrap items-center gap-2">
                <form method="post" action=(&deploy_url) @submit=$(|_e: Event| {
                    deploying.set(true);
                    deploy_note.set("Starting a deployment…".to_owned());
                })>
                    button(
                        attrs: attributes! {
                            type="submit"
                            :disabled=$(if can_deploy { busy.get() } else { true })
                        },
                        icon(data: iconify_icon!("lucide:rocket"))
                        "Deploy"
                    )
                </form>
                if release.is_some() {
                    button(
                        variant: ButtonVariant::Outline,
                        attrs: attributes! {
                            type="button"
                            :disabled=$(busy.get())
                            @click=$(async |_e: Event| {
                                restarting.set(true);
                                restart_note.set(restart_doing.to_owned());
                                let outcome = restart_app(project_id, service_id).await;
                                restart_failed.set(!outcome.0);
                                restart_note.set(outcome.1);
                                restarting.set(false);
                                restart_done.increment();
                            })
                        },
                        if stopped {
                            icon(data: iconify_icon!("lucide:play"))
                            "Start"
                        } else {
                            icon(data: iconify_icon!("lucide:rotate-cw"))
                            "Restart"
                        }
                    )
                    if !stopped {
                        button(
                            variant: ButtonVariant::Outline,
                            attrs: attributes! {
                                type="button"
                                class="hover:text-destructive"
                                :disabled=$(busy.get())
                                @click=$(async |_e: Event| {
                                    stopping.set(true);
                                    stop_note.set("Stopping…".to_owned());
                                    let outcome = stop_app(project_id, service_id).await;
                                    stop_failed.set(!outcome.0);
                                    stop_note.set(outcome.1);
                                    stopping.set(false);
                                    stop_done.increment();
                                })
                            },
                            icon(data: iconify_icon!("lucide:square"))
                            "Stop"
                        )
                    }
                }
                <p
                    role="status"
                    :hidden=$(note.get().is_empty())
                    :class=$(if busy.get() {
                        "flex items-center gap-1.5 text-sm text-muted-foreground"
                    } else if failed.get() {
                        "flex items-center gap-1.5 text-sm text-destructive"
                    } else {
                        "flex items-center gap-1.5 text-sm text-emerald-700 dark:text-emerald-400"
                    })
                >
                    <span class="inline-flex animate-spin" :hidden=$(!busy.get())>
                        icon(data: iconify_icon!("lucide:loader-circle"), size: 14)
                    </span>
                    <span class="inline-flex" :hidden=$(if busy.get() { true } else { failed.get() })>
                        icon(data: iconify_icon!("lucide:circle-check"), size: 14)
                    </span>
                    <span class="inline-flex" :hidden=$(if busy.get() { true } else { !failed.get() })>
                        icon(data: iconify_icon!("lucide:circle-x"), size: 14)
                    </span>
                    $(note.get())
                </p>
            </div>
            if !has_source {
                field_description("Save a source below to deploy.")
            }
            if let Some(release) = &release {
                <p class="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted-foreground">
                    <span class="font-medium text-foreground">"Release #" (release.id.to_string())</span>
                    if let Some(tag) = &release.tag {
                        <span class="font-mono text-xs">(tag)</span>
                    } else if let Some(commit) = &release.commit {
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

/// The service with this ID in the project with this ID, if both belong to the
/// organization the browser works in, as a procedure or a shard, which answer requests
/// of their own, must check.
async fn find_service(cx: &Cx, project_id: u64, service_id: u64) -> Result<(Project, Service)> {
    let organization = require_organization(cx).await?;
    let project = Project::find_in(cx, organization.id, project_id)
        .await?
        .ok_or_not_found()?;
    let service = Service::find_in(cx, project.id, service_id)
        .await?
        .ok_or_not_found()?;
    Ok((project, service))
}

/// What a button did, for the deploy card: whether it worked, and what to say, such as
/// `Restarted at 14:05:09`.
fn outcome(done: &str) -> (bool, String) {
    let time = time_zone::at(jiff::Timestamp::now()).strftime("%H:%M:%S").to_string();
    (true, format!("{done} at {time}"))
}

/// Starts the service's release again, whether it was stopped or running, from the
/// deploy card, and says what became of it.
#[procedure]
async fn restart_app(cx: &Cx, project_id: u64, service_id: u64) -> Result<(bool, String)> {
    let (_, mut service) = find_service(cx, project_id, service_id).await?;
    let was_stopped = service.stopped_at.is_some();
    Ok(match Deployer::get(cx).restart(&mut service).await? {
        Ok(()) if was_stopped => outcome("Started"),
        Ok(()) => outcome("Restarted"),
        Err(error) => (false, format!("The app could not start: {error}")),
    })
}

/// Stops the service until it is started or deployed again, from the deploy card, and
/// says so.
#[procedure]
async fn stop_app(cx: &Cx, project_id: u64, service_id: u64) -> Result<(bool, String)> {
    let (_, mut service) = find_service(cx, project_id, service_id).await?;
    Deployer::get(cx).stop(&mut service).await?;
    Ok(outcome("Stopped"))
}

/// What the GitHub release tab's fields offer to choose from.
enum ReleaseChoices {
    /// Asked for once the browser connects, so the page does not wait for GitHub.
    Loading,
    /// The service has no source saved yet.
    NoSource,
    /// The repository has no release yet.
    NoRelease(String),
    Failed(String),
    Found {
        app: Box<GithubApp>,
        repository: String,
        release: Release,
    },
}

/// The service, and the latest release of its saved repository, for the GitHub release
/// tab's fields.
///
/// Shards answer requests of their own, without the page's guards, so this checks
/// access.
async fn release_choices(cx: &Cx, project_id: u64, service_id: u64) -> Result<(Service, ReleaseChoices)> {
    let organization = require_organization(cx).await?;
    let project = Project::find_in(cx, organization.id, project_id)
        .await?
        .ok_or_not_found()?;
    let service = Service::find_in(cx, project.id, service_id)
        .await?
        .ok_or_not_found()?;
    let (Some(app_id), Some(repository)) = (service.github_app_id, service.repository.clone())
    else {
        return Ok((service, ReleaseChoices::NoSource));
    };
    let app = GithubApp::find_in(cx, organization.id, app_id).await?;
    let Some(app) = app.filter(|app| app.installation_id.is_some()) else {
        return Ok((service, ReleaseChoices::NoSource));
    };
    if !connected(cx) {
        return Ok((service, ReleaseChoices::Loading));
    }
    let choices = match app.latest_release(&repository).await {
        Ok(Some(release)) => ReleaseChoices::Found {
            app: Box::new(app),
            repository,
            release,
        },
        Ok(None) => ReleaseChoices::NoRelease(repository),
        Err(error) => ReleaseChoices::Failed(error.to_string()),
    };
    Ok((service, choices))
}

/// The files of the latest release of the service's repository, to choose the one to
/// deploy, as it is saved: with the release's version as `*`. Choosing one updates
/// `file`, which lists the binaries in it. Without a release to choose from, the name is
/// typed.
#[shard]
async fn release_file_field(
    cx: &Cx,
    project_id: u64,
    service_id: u64,
    file: Signal<String>,
) -> Result<impl View> {
    let (_, choices) = release_choices(cx, project_id, service_id).await?;
    let saved = file.get_untracked();
    let id = element_id(cx);
    let (choose, type_in) = (file.clone(), file.clone());
    let reason = match &choices {
        ReleaseChoices::NoSource => "Save a source above to choose from its releases. Meanwhile, type the file's name.".to_owned(),
        ReleaseChoices::NoRelease(repository) => format!("{repository} has no release yet. Type the file's name, or publish a release and come back to choose it."),
        ReleaseChoices::Failed(error) => format!("{error}. Type the file's name meanwhile."),
        ReleaseChoices::Loading | ReleaseChoices::Found { .. } => String::new(),
    };

    Ok(view! {
        field(
            field_label(attrs: attributes! { for=(&id) }, "File")
            match &choices {
                ReleaseChoices::Found { release, .. } => {
                    let options = release_file_options(release, &saved);
                    select(
                        attrs: attributes! {
                            id=(&id)
                            name="file"
                            required=""
                            @change=$(|e: Event| choose.set(e.target.value))
                        },
                        // With `required`, this empty first option is the placeholder.
                        <option value="" selected=(saved.is_empty().then_some(""))>"Choose a file"</option>
                        #[key(value.clone())]
                        for (value, label) in &options {
                            <option value=(value) selected=((*value == saved).then_some(""))>(label)</option>
                        }
                    )
                    field_description(
                        "From release " <span class="font-mono">(&release.tag_name)</span>
                        ". A version in a name is saved as *, so the next releases match too. When the release also has the file's .sha256, oxploy checks the download against it."
                    )
                }
                ReleaseChoices::Loading => {
                    select(
                        attrs: attributes! { id=(&id) name="file" required="" },
                        <option value=(&saved) selected="">
                            (if saved.is_empty() { "Loading the release from GitHub…" } else { saved.as_str() })
                        </option>
                    )
                    field_description("Loading the latest release from GitHub…")
                }
                _ => {
                    input(
                        attrs: attributes! {
                            id=(&id)
                            name="file"
                            value=(&saved)
                            placeholder="api-x86_64-linux.tar.gz"
                            maxlength="255"
                            autocomplete="off"
                            spellcheck="false"
                            required=""
                            @change=$(|e: Event| type_in.set(e.target.value))
                        }
                    )
                    field_description((reason) " A * stands for any text, such as the version in api-*-linux.tar.gz.")
                }
            }
        )
    })
}

/// The files of `release` to deploy, as the value to save and a label with the file's
/// name and size, the saved one kept when the release no longer has it.
fn release_file_options(release: &Release, saved: &str) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> = release
        .assets
        .iter()
        .filter(|file| file.state == "uploaded" && !file.name.ends_with(".sha256"))
        .map(|file| {
            let mut label = format!("{} · {:.1} MB", file.name, file.size as f64 / 1_000_000.0);
            if named_for_this_machine(&file.name) {
                label.push_str(" · this server");
            }
            (release_file_pattern(&release.tag_name, &file.name), label)
        })
        .collect();
    if !saved.is_empty() && !options.iter().any(|(value, _)| value == saved) {
        options.insert(0, (saved.to_owned(), format!("{saved} · saved")));
    }
    options
}

/// What the binary field offers, for the file chosen.
enum BinaryChoices {
    NoFile,
    /// The file is not an archive, so it is the binary.
    TheFile,
    Loading,
    Found(Vec<Runnable>),
    /// Typed, for this reason.
    Typed(String),
}

/// The binaries in the chosen file, when it is an archive, to choose the one to start.
/// oxploy downloads the archive to look into it. When it cannot, the path is typed,
/// starting from a guess.
#[shard]
async fn release_binary_field(cx: &Cx, project_id: u64, service_id: u64, file: String) -> Result<impl View> {
    let (service, choices) = release_choices(cx, project_id, service_id).await?;
    let id = element_id(cx);
    let file = file.trim().to_owned();
    let saved = service.github_asset_binary.clone().unwrap_or_default();
    let binaries = if file.is_empty() {
        BinaryChoices::NoFile
    } else if !archive::is_archive(&file) {
        BinaryChoices::TheFile
    } else {
        match &choices {
            ReleaseChoices::Loading => BinaryChoices::Loading,
            ReleaseChoices::Found { app, repository, release } => {
                let found = release.assets.iter().find(|asset| {
                    asset.state == "uploaded" && release_file_matches(&file, &asset.name)
                });
                match found {
                    Some(asset) => match app.runnables_in_release_file(repository, asset).await {
                        Ok(runnables) if !runnables.is_empty() => BinaryChoices::Found(runnables),
                        Ok(_) => BinaryChoices::Typed(format!("{} has no file it can run.", asset.name)),
                        Err(error) => BinaryChoices::Typed(format!("The archive could not be looked into: {error}.")),
                    },
                    None => BinaryChoices::Typed(format!("Release {} has no file named {file}.", release.tag_name)),
                }
            }
            _ => BinaryChoices::Typed(String::new()),
        }
    };
    let guess = guess_binary(&file);
    let this = Machine::this();
    let runs_here = |runnable: &&Runnable| runnable.machine.is_none_or(|machine| machine.runs_on(this));
    let named = |runnable: &&Runnable| runnable.path.rsplit('/').next() == Some(guess.as_str());
    let chosen = match &binaries {
        BinaryChoices::Found(runnables) if runnables.iter().any(|runnable| runnable.path == saved) => saved.clone(),
        // The guess if it runs here, or else any that runs here, the guess, or the first.
        BinaryChoices::Found(runnables) => runnables
            .iter()
            .find(|runnable| runs_here(runnable) && named(runnable))
            .or_else(|| runnables.iter().find(runs_here))
            .or_else(|| runnables.iter().find(named))
            .or(runnables.first())
            .map(|runnable| runnable.path.clone())
            .unwrap_or_default(),
        _ if saved.is_empty() => guess.clone(),
        _ => saved.clone(),
    };
    // The chosen binary's machine, when it is not this one.
    let elsewhere = match &binaries {
        BinaryChoices::Found(runnables) => runnables
            .iter()
            .find(|runnable| runnable.path == chosen)
            .and_then(|runnable| runnable.machine)
            .filter(|machine| !machine.runs_on(this)),
        _ => None,
    };

    Ok(view! {
        field(
            field_label(attrs: attributes! { for=(&id) }, "Binary")
            match &binaries {
                BinaryChoices::NoFile => {
                    select(
                        attrs: attributes! { id=(&id) name="binary" disabled="" },
                        <option>"Choose a file first"</option>
                    )
                }
                BinaryChoices::TheFile => {
                    <input type="hidden" name="binary" value="">
                    <p id=(&id) class="flex min-h-10 items-center text-sm text-muted-foreground">
                        "The file is the binary itself."
                    </p>
                }
                BinaryChoices::Loading => {
                    select(
                        attrs: attributes! { id=(&id) name="binary" required="" },
                        <option value=(&saved) selected="">
                            (if saved.is_empty() { "Looking in the archive…" } else { saved.as_str() })
                        </option>
                    )
                    field_description("Looking in the archive…")
                }
                BinaryChoices::Found(runnables) => {
                    select(
                        attrs: attributes! { id=(&id) name="binary" required="" },
                        #[key(runnable.path.clone())]
                        for runnable in runnables {
                            <option value=(&runnable.path) selected=((runnable.path == chosen).then_some(""))>
                                (&runnable.path)
                                if let Some(machine) = runnable.machine {
                                    " · " (machine.to_string())
                                    if !machine.runs_on(this) {
                                        ", not this server"
                                    }
                                }
                            </option>
                        }
                    )
                    if let Some(machine) = elsewhere {
                        <p class="flex items-start gap-2 text-sm text-destructive">
                            icon(data: iconify_icon!("lucide:triangle-alert"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0" })
                            <span>
                                (format!("{chosen} is built for {machine}, and this server is {this}, so it would not start here. Choose the release's file built for this server, or publish one."))
                            </span>
                        </p>
                    } else {
                        field_description("The files in the archive that can run. The binary runs with the files beside it, such as a Topcoat app's assets.")
                    }
                }
                BinaryChoices::Typed(reason) => {
                    input(
                        attrs: attributes! {
                            id=(&id)
                            name="binary"
                            value=(&chosen)
                            placeholder="api"
                            maxlength="255"
                            autocomplete="off"
                            spellcheck="false"
                            required=""
                        }
                    )
                    field_description((reason) " The binary in the .tar.gz, such as api or bin/api.")
                }
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

/// A form with the file of a GitHub release to deploy.
#[derive(Deserialize)]
struct ReleaseForm {
    file: String,
    #[serde(default)]
    binary: String,
    /// `on` when new releases deploy on their own.
    automatic: Option<String>,
}

/// Makes the service deploy a file of its repository's GitHub releases.
#[route(POST "./release")]
async fn save_release(cx: &Cx, Form(form): Form<ReleaseForm>) -> Result<SeeOther> {
    let (project, mut service) = current_service(cx).await?;
    let build = ReleaseBuild {
        file: &form.file,
        binary: &form.binary,
        automatic: form.automatic.is_some(),
    };
    if !service.set_release(cx, &build).await? {
        return Err(bad_request(
            "Name the release's file without slashes, and for a .tar.gz the binary in it, as a path inside the archive",
        )
        .into());
    }
    Ok(see_other(general_url(cx, &project, &service)))
}

/// A form with how Docker builds and runs the service.
#[derive(Deserialize)]
struct DockerfileForm {
    #[serde(default)]
    dockerfile: String,
    #[serde(default)]
    port: String,
}

/// Makes the service build with Docker and run in a container.
#[route(POST "./dockerfile")]
async fn save_dockerfile(cx: &Cx, Form(form): Form<DockerfileForm>) -> Result<SeeOther> {
    let (project, mut service) = current_service(cx).await?;
    if let Err(why) = docker::status().await {
        return Err(bad_request(why.to_string()).into());
    }
    let build = DockerfileBuild {
        dockerfile: &form.dockerfile,
        port: &form.port,
    };
    if !service.set_dockerfile(cx, &build).await? {
        return Err(bad_request(
            "Name the Dockerfile as a path inside the repository, and the port as a number from 1 to 65535",
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

/// The service's page, which opens on this tab.
fn general_url(cx: &Cx, project: &Project, service: &Service) -> String {
    href!(super::page, ProjectId(project.id), ServiceId(service.id)).resolve(cx)
}
