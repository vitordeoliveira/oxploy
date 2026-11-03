//! What the pages about domains share: whether Caddy runs oxploy's routes, and where a
//! name points.

use topcoat::{
    Result,
    icon::{icon, iconify::iconify_icon},
    view::{View, attributes, class, component, view},
};

use crate::features::caddy::CaddyStatus;
use crate::features::domain::Resolution;

/// What keeps the domains from working, if Caddy does not run oxploy's routes.
#[component]
pub(super) async fn caddy_notice(status: &CaddyStatus, admin: &str) -> Result<impl View> {
    let problem = match status {
        CaddyStatus::Unreachable(error) => Some((
            "Caddy is not running".to_owned(),
            format!(
                "The domains work once Caddy runs on this machine. Install it and run it as a service; oxploy gives it the routes on its own, through its admin API at {admin}."
            ),
            error.clone(),
        )),
        CaddyStatus::Refused(message) => Some((
            "Caddy refused the routes".to_owned(),
            "Caddy runs, but would not load the routes oxploy gave it.".to_owned(),
            message.clone(),
        )),
        CaddyStatus::Unknown | CaddyStatus::Running => None,
    };

    Ok(view! {
        if let Some((title, text, detail)) = problem {
            <div class="flex items-start gap-3 rounded-lg border border-amber-500/40 bg-amber-500/10 p-4 text-sm">
                icon(
                    data: iconify_icon!("lucide:triangle-alert"),
                    size: 16,
                    attrs: attributes! { class="mt-0.5 shrink-0 text-amber-700 dark:text-amber-400" }
                )
                <div class="flex min-w-0 flex-col gap-1">
                    <p class="font-medium">(title)</p>
                    <p class="text-muted-foreground">(text)</p>
                    <p class="font-mono text-xs break-all text-muted-foreground">(detail)</p>
                </div>
            </div>
        }
    })
}

/// Where `host` points, or that it is being looked up while `resolution` is `None`.
#[component]
pub(super) async fn resolution_label(host: &str, resolution: Option<&Resolution>) -> Result<impl View> {
    let (symbol, color, text) = match resolution {
        None => (iconify_icon!("lucide:loader"), "text-muted-foreground", "checking DNS…".to_owned()),
        Some(Resolution::Here) => (
            iconify_icon!("lucide:circle-check"),
            "text-emerald-600 dark:text-emerald-400",
            "points here".to_owned(),
        ),
        Some(Resolution::Elsewhere(addresses)) => (
            iconify_icon!("lucide:triangle-alert"),
            "text-amber-700 dark:text-amber-400",
            format!(
                "points to {}",
                addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
            ),
        ),
        Some(Resolution::Nowhere) => (
            iconify_icon!("lucide:triangle-alert"),
            "text-amber-700 dark:text-amber-400",
            "has no DNS record".to_owned(),
        ),
    };

    Ok(view! {
        <span class=(class!("inline-flex items-center gap-1", color))>
            icon(data: symbol, size: 12, attrs: attributes! { class="shrink-0" })
            <span class="break-all">(host) " " (text)</span>
        </span>
    })
}
