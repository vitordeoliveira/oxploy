//! Updating oxploy from the panel: a button at the foot of the sidebar once a newer
//! release is out, the dialog it opens, which installs the release and shows how far it
//! got, and the routes they and the settings page use.

use std::time::Duration;

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{IconData, icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{SeeOther, bad_request},
        href, route,
    },
    runtime::{Event, Signal, connected, shard},
    view::{View, attributes, component, element_id, emit, live, view},
};

use super::projects::created_at;
use super::{back, require_server_owner, require_sign_in};
use crate::app::dashboard::projects::project_id::services::service_id::settings_card;
use crate::components::button::{ButtonVariant, button};
use crate::components::dialog::{dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title};
use crate::components::sidebar::{SidebarMenuButtonVariant, sidebar_menu, sidebar_menu_button, sidebar_menu_item};
use crate::features::deploy::Deployer;
use crate::features::update::{Blocker, Installation, Release, Step, Updater, VERSION};

/// How often an open page shows how far an update got, while it installs.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// The button at the foot of the sidebar that opens the update dialog, once a newer
/// release is out or while one installs. A shard answers requests of its own, so it
/// checks access itself.
#[shard]
pub(super) async fn update_button(cx: &Cx, open: Signal<bool>) -> Result<impl View> {
    require_server_owner(cx).await?;

    Ok(view! {
        (live! {
            loop {
                let updater = Updater::get(cx);
                let available = updater.available();
                let installation = updater.installation();
                let token = emit! {
                    update_button_view(open: &open, available: available.as_ref(), installation: installation.as_ref())
                }?;
                if !connected(cx) || !updater.is_installing() {
                    break Ok(token);
                }
                tokio::time::sleep(PROGRESS_INTERVAL).await;
            }
        })
    })
}

#[component]
async fn update_button_view(
    open: &Signal<bool>,
    available: Option<&Release>,
    installation: Option<&Installation>,
) -> Result<impl View> {
    let open = open.clone();
    let running = installation.is_some_and(Installation::is_running);
    let (symbol, label, symbol_class) = match installation {
        Some(_) if running => (iconify_icon!("lucide:loader-circle"), "Updating…", "animate-spin text-primary"),
        Some(_) => (iconify_icon!("lucide:triangle-alert"), "Update failed", "text-destructive"),
        None => (iconify_icon!("lucide:download"), "Update available", ""),
    };

    Ok(view! {
        if installation.is_some() || available.is_some() {
            sidebar_menu(
                sidebar_menu_item(
                    sidebar_menu_button(
                        variant: SidebarMenuButtonVariant::Outline,
                        tooltip: Some(label),
                        attrs: attributes! {
                            class="relative"
                            data-update-running=(running.then_some(""))
                            @click=$(|_e: Event| open.set(true))
                        },
                        icon(data: symbol, attrs: attributes! { class=(symbol_class) })
                        if installation.is_none() {
                            // A dot that pulses, as long as the update waits. The label
                            // stays the last child, which the collapsed sidebar hides.
                            <span class="absolute top-1/2 right-3 flex size-2 -translate-y-1/2 md:group-data-[collapsible=icon]/sidebar:top-1 md:group-data-[collapsible=icon]/sidebar:right-1 md:group-data-[collapsible=icon]/sidebar:translate-y-0">
                                <span class="absolute inline-flex size-full rounded-full bg-emerald-400 opacity-75 motion-safe:animate-ping"></span>
                                <span class="relative inline-flex size-2 rounded-full bg-emerald-500"></span>
                            </span>
                        }
                        <span>(label)</span>
                    )
                )
            )
        }
    })
}

/// The dialog that installs the newer release and shows how far it got, open while
/// `open` is. A shard answers requests of its own, so it checks access itself.
#[shard]
pub(super) async fn update_dialog(cx: &Cx, open: Signal<bool>, return_to: String) -> Result<impl View> {
    require_server_owner(cx).await?;
    let shown = open.clone();
    let title_id = element_id(cx);
    let install_url = href!(install).resolve(cx);

    Ok(view! {
        dialog(
            open: $(shown.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                (live! {
                    loop {
                        let updater = Updater::get(cx);
                        let available = updater.available();
                        let blocker = available.as_ref().and_then(|release| updater.blocker(release));
                        let installation = updater.installation();
                        let token = emit! {
                            update_details(
                                open: &open,
                                title_id: &title_id,
                                available: available.as_ref(),
                                blocker: blocker.as_ref(),
                                installation: installation.as_ref(),
                                install_url: &install_url,
                                return_to: &return_to
                            )
                        }?;
                        if !connected(cx) || !updater.is_installing() {
                            break Ok(token);
                        }
                        tokio::time::sleep(PROGRESS_INTERVAL).await;
                    }
                })
            )
        )
    })
}

/// What the dialog says: the newer release and how to install it, how far its
/// installation got, or why it failed.
#[component]
async fn update_details(
    open: &Signal<bool>,
    title_id: &str,
    available: Option<&Release>,
    blocker: Option<&Blocker>,
    installation: Option<&Installation>,
    install_url: &str,
    return_to: &str,
) -> Result<impl View> {
    let close = open.clone();
    let running = installation.filter(|installation| installation.is_running());
    let failed = installation.and_then(|installation| match &installation.step {
        Step::Failed(error) => Some((installation.version.as_str(), error.as_str())),
        _ => None,
    });
    let installable = available.filter(|_| blocker.is_none() && running.is_none());
    let title = match (running, failed, available) {
        (Some(installation), _, _) => format!("Updating to v{}", installation.version),
        (None, Some((version, _)), _) => format!("The update to v{version} failed"),
        (None, None, Some(_)) => "Update oxploy".to_owned(),
        (None, None, None) => "oxploy is up to date".to_owned(),
    };

    Ok(view! {
        dialog_header(
            dialog_title(attrs: attributes! { id=(title_id) }, (title))
            if let Some(release) = available {
                <p class="flex flex-wrap items-center gap-2 font-mono text-sm">
                    <span class="text-muted-foreground">"v" (VERSION)</span>
                    icon(data: iconify_icon!("lucide:arrow-right"), size: 14, attrs: attributes! { class="text-muted-foreground" })
                    <span class="font-medium">"v" (&release.version)</span>
                </p>
            }
        )
        if let Some(installation) = running {
            <div data-update-running="" class="flex flex-col gap-4">
                dialog_description(
                    "The panel restarts on the new version, and this page reloads once it is back. The apps keep running."
                )
                progress(step: &installation.step, version: &installation.version)
            </div>
        } else {
            if let Some((_, error)) = failed {
                <div class="flex items-start gap-3 rounded-lg border border-destructive/40 bg-destructive/10 p-4 text-sm">
                    icon(data: iconify_icon!("lucide:circle-x"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0 text-destructive" })
                    <p class="min-w-0 break-words">(sentence(error))</p>
                </div>
            }
            if let Some(release) = available {
                dialog_description(
                    "The panel restarts on the new version, which takes a few seconds, and this page reloads once it is back. The apps keep running, and deployments in progress finish first. The "
                    <a href=(&release.url) target="_blank" rel="noreferrer" class="font-medium text-foreground underline underline-offset-4">"release notes"</a>
                    " tell what changed."
                )
                if let Some(blocker) = blocker {
                    blocker_notice(blocker: blocker, release: release)
                }
            } else if failed.is_none() {
                dialog_description("The panel runs v" (VERSION) ", the latest release.")
            }
        }
        dialog_footer(
            button(
                variant: ButtonVariant::Ghost,
                attrs: attributes! { type="button" @click=$(|_e: Event| close.set(false)) },
                (if running.is_some() { "Hide" } else { "Close" })
            )
            if installable.is_some() {
                <form method="post" action=(install_url)>
                    <input type="hidden" name="return_to" value=(return_to)>
                    button(
                        attrs: attributes! { type="submit" },
                        icon(data: iconify_icon!("lucide:download"))
                        (if failed.is_some() { "Try again" } else { "Update now" })
                    )
                </form>
            }
        )
    })
}

/// `text` with a capital first letter. Errors read as clauses, which a message alone
/// starts as a sentence.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The steps of an installation, with the one it is at.
#[component]
async fn progress(step: &Step, version: &str) -> Result<impl View> {
    let at = match step {
        Step::Downloading => 0,
        Step::Checking => 1,
        Step::WaitingForDeployments(_) | Step::Restarting => 2,
        Step::Failed(_) => 3,
    };
    let restart = match step {
        Step::WaitingForDeployments(1) => "Waiting for a deployment to finish".to_owned(),
        Step::WaitingForDeployments(count) => format!("Waiting for {count} deployments to finish"),
        _ => format!("Restart on v{version}"),
    };
    let steps = [
        format!("Download v{version}"),
        "Check it against the release".to_owned(),
        restart,
    ];

    Ok(view! {
        <ol class="flex flex-col gap-2.5 text-sm">
            #[key(index)]
            for (index, text) in steps.iter().enumerate() {
                <li
                    class=(if index == at { "flex items-center gap-2.5 font-medium" } else if index < at { "flex items-center gap-2.5 text-muted-foreground" } else { "flex items-center gap-2.5 text-muted-foreground/70" })
                    aria-current=((index == at).then_some("step"))
                >
                    step_icon(state: index.cmp(&at))
                    (text)
                </li>
            }
        </ol>
    })
}

/// A step's mark: done, under way, or still to come.
#[component]
async fn step_icon(state: std::cmp::Ordering) -> Result<impl View> {
    let (symbol, class): (IconData, &str) = match state {
        std::cmp::Ordering::Less => (iconify_icon!("lucide:circle-check"), "shrink-0 text-emerald-600 dark:text-emerald-400"),
        std::cmp::Ordering::Equal => (iconify_icon!("lucide:loader-circle"), "shrink-0 animate-spin text-primary"),
        std::cmp::Ordering::Greater => (iconify_icon!("lucide:circle"), "shrink-0"),
    };

    Ok(view! {
        icon(data: symbol, size: 16, attrs: attributes! { class=(class) })
    })
}

/// Why the panel cannot install the release on its own, and what to do instead.
#[component]
async fn blocker_notice(blocker: &Blocker, release: &Release) -> Result<impl View> {
    let user = std::env::var("USER").unwrap_or_else(|_| "oxploy".to_owned());
    let (title, text, command) = match blocker {
        Blocker::DevelopmentBuild => (
            "This is a development build".to_owned(),
            "It runs from its code, so update the code instead, such as with git pull.".to_owned(),
            None,
        ),
        Blocker::NoBuild(machine) => (
            "No build for this machine".to_owned(),
            format!("v{} has no build for {machine}. Build oxploy from its code instead.", release.version),
            None,
        ),
        Blocker::ReadOnly(folder) => (
            "oxploy cannot change its folder".to_owned(),
            format!(
                "It is installed in {}, which its user, {user}, cannot change. Give the folder to the user once, then update from here:",
                folder.display()
            ),
            Some(format!("sudo chown -R {user}: {}", folder.display())),
        ),
        Blocker::NoBinary => (
            "oxploy cannot find its binary".to_owned(),
            "Update it by hand, as it was installed.".to_owned(),
            None,
        ),
    };

    Ok(view! {
        <div class="flex items-start gap-3 rounded-lg border border-amber-500/40 bg-amber-500/10 p-4 text-sm">
            icon(
                data: iconify_icon!("lucide:triangle-alert"),
                size: 16,
                attrs: attributes! { class="mt-0.5 shrink-0 text-amber-700 dark:text-amber-400" }
            )
            <div class="flex min-w-0 flex-col gap-1">
                <p class="font-medium">(title)</p>
                <p class="text-muted-foreground">(text)</p>
                if let Some(command) = command {
                    <code class="mt-1 rounded-md bg-muted px-2 py-1.5 font-mono text-xs break-all">(command)</code>
                }
            </div>
        </div>
    })
}

/// What the forms here send back: the page they were on.
#[derive(Deserialize)]
struct ReturnForm {
    return_to: String,
}

/// Starts installing the newer release.
#[route(POST "./install")]
async fn install(cx: &Cx, Form(form): Form<ReturnForm>) -> Result<SeeOther> {
    require_server_owner(cx).await?;
    let updater = Updater::get(cx);
    let Some(release) = updater.available() else {
        return Err(bad_request("No newer release to install").into());
    };
    updater.install(&release, Deployer::get(cx).clone());
    Ok(back(cx, &form.return_to))
}

/// Asks GitHub for the latest release now.
#[route(POST "./check")]
async fn check(cx: &Cx, Form(form): Form<ReturnForm>) -> Result<SeeOther> {
    require_server_owner(cx).await?;
    Updater::get(cx).check_now().await;
    Ok(back(cx, &form.return_to))
}

/// The version the panel runs. Pages ask for it while an update installs, and reload
/// once another version answers.
#[route(GET "./version")]
async fn running_version(cx: &Cx) -> Result<&'static str> {
    require_sign_in(cx).await?;
    Ok(VERSION)
}

/// The URL of [`running_version`], for the script that reloads pages after an update.
pub(super) fn version_url(cx: &Cx) -> String {
    href!(running_version).resolve(cx)
}

/// The settings page's card about the running version and the latest release, with
/// buttons that look for a newer one and that open `open`'s dialog to install it.
#[component]
pub(super) async fn version_card(cx: &Cx, open: &Signal<bool>, return_to: &str) -> Result<impl View> {
    let updater = Updater::get(cx);
    let check_result = updater.check();
    let available = updater.available();
    let open = open.clone();
    let check_url = href!(check).resolve(cx);

    Ok(view! {
        settings_card(
            title: "Version",
            text: "oxploy looks for a new release on GitHub every hour.",
            <dl class="grid grid-cols-1 gap-x-6 gap-y-3 text-sm sm:grid-cols-[10rem_minmax(0,1fr)]">
                <dt class="font-medium">"Running"</dt>
                <dd class="text-muted-foreground">
                    "v" (VERSION)
                    if check_result.latest.is_some() && available.is_none() {
                        <span class="text-emerald-600 dark:text-emerald-400">" · up to date"</span>
                    }
                </dd>
                <dt class="font-medium">"Latest release"</dt>
                <dd class="text-muted-foreground">
                    if let Some(release) = &check_result.latest {
                        <a
                            href=(&release.url)
                            target="_blank"
                            rel="noreferrer"
                            class="inline-flex items-center gap-1 hover:text-foreground hover:underline"
                        >
                            "v" (&release.version)
                            icon(data: iconify_icon!("lucide:external-link"), size: 12)
                        </a>
                    } else {
                        "Not known yet"
                    }
                </dd>
                <dt class="font-medium">"Checked"</dt>
                <dd class="text-muted-foreground">
                    if let Some(at) = check_result.at {
                        created_at(seconds: at)
                    } else {
                        "Not yet"
                    }
                </dd>
            </dl>
            if let Some(error) = &check_result.error {
                <p class="flex items-start gap-2 text-sm text-destructive">
                    icon(data: iconify_icon!("lucide:triangle-alert"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0" })
                    <span class="min-w-0 break-words">"The last check failed: " (error)</span>
                </p>
            }
            <div class="flex flex-wrap gap-2">
                <form method="post" action=(&check_url)>
                    <input type="hidden" name="return_to" value=(return_to)>
                    button(
                        variant: ButtonVariant::Outline,
                        attrs: attributes! { type="submit" },
                        icon(data: iconify_icon!("lucide:refresh-cw"))
                        "Check for updates"
                    )
                </form>
                if let Some(release) = &available {
                    button(
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(true)) },
                        icon(data: iconify_icon!("lucide:download"))
                        "Update to v" (&release.version)
                    )
                }
            </div>
        )
    })
}
