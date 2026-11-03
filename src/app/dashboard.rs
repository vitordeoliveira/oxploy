//! The signed-in area: a sidebar beside the page.

pub mod git;
pub mod monitoring;
mod organizations;
pub mod projects;

use topcoat::{
    Result,
    asset::asset,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        Slot,
        error::{SeeOther, redirect, see_other},
        href, layout, page,
        request::uri,
    },
    runtime::{Event, signal},
    view::{View, attributes, component, view},
};

use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::separator::{SeparatorOrientation, separator};
use crate::components::sidebar::{
    SidebarCollapsible, SidebarVariant, sidebar, sidebar_content, sidebar_footer, sidebar_group,
    sidebar_group_label, sidebar_header, sidebar_inset, sidebar_menu, sidebar_menu_button,
    sidebar_menu_item, sidebar_provider, sidebar_rail, sidebar_trigger,
};
use crate::features::organization::Organization;
use crate::features::session::{SignedIn, UserSession};
use crate::features::user::{Role, User};
use organizations::{OrganizationMenu, organization_dialogs, organization_switcher};

/// Returns who is signed in, or sends the browser to sign in.
///
/// The layout below guards the pages, but routes skip layouts, so each route calls this
/// too.
pub async fn require_sign_in(cx: &Cx) -> Result<SignedIn> {
    match UserSession::current(cx).await? {
        Some(signed_in) => Ok(signed_in),
        None => Err(see_other(href!(super::login::page).resolve(cx)).into()),
    }
}

/// Returns the organization this browser is working in, or sends the browser to sign
/// in.
pub async fn require_organization(cx: &Cx) -> Result<Organization> {
    let mut signed_in = require_sign_in(cx).await?;
    let mut organizations = signed_in.organizations(cx).await?;
    Ok(organizations.all.swap_remove(organizations.active))
}

/// Sends the browser back to the dashboard page a form was on. Anything else, such as
/// another site, goes to the projects instead.
fn back(cx: &Cx, return_to: &str) -> SeeOther {
    let is_dashboard_page = return_to.starts_with("/dashboard")
        && !return_to.contains("//")
        && !return_to.contains('\\');
    if is_dashboard_page {
        see_other(return_to)
    } else {
        see_other(href!(projects::page).resolve(cx))
    }
}

/// `/dashboard` has no page of its own yet, so it opens the projects.
#[page]
async fn page(cx: &Cx) -> Result<()> {
    Err(redirect(href!(projects::page).resolve(cx)).into())
}

/// Marks the current page's menu button with a rust bar along its left edge.
const CURRENT_PAGE: &str = "relative before:absolute before:inset-y-2 before:left-0 before:w-0.75 before:rounded-full before:bg-sidebar-primary before:opacity-0 data-[active=true]:before:opacity-100 data-[active=true]:[&>svg]:text-sidebar-primary";

#[layout]
async fn layout(cx: &Cx, slot: Slot<'_>) -> Result<impl View> {
    let mut signed_in = require_sign_in(cx).await?;
    let organizations = signed_in.organizations(cx).await?;
    let organization = organizations.all[organizations.active].name.clone();
    let menu = OrganizationMenu::new(cx, organizations, uri(cx).path().to_owned());

    // Whether the sidebar is expanded on desktop, and whether it is shown on mobile.
    let open = signal(cx, || true);
    let mobile_open = signal(cx, || false);
    let projects = href!(projects::page);
    let projects_url = projects.resolve(cx);
    let git_url = href!(git::page).resolve(cx);
    let monitoring_url = href!(monitoring::page).resolve(cx);
    let oxploy_url = href!(monitoring::oxploy::page).resolve(cx);
    // Pages about the whole server are only for the user who owns it.
    let owns_server = signed_in.user.role == Role::SuperAdmin;
    let path = uri(cx).path();
    let within = |section: &str| path == section || path.starts_with(&format!("{section}/"));
    let on_server_page = within(&monitoring_url);
    let in_projects = within(&projects_url);
    let in_git = within(&git_url);
    // The breadcrumb names what the page is about: the whole server, or something of
    // the active organization, down to the open project and service.
    let trail = if on_server_page {
        let mut trail = vec![("Monitoring".to_owned(), monitoring_url.clone())];
        if within(&oxploy_url) {
            trail.push(("oxploy".to_owned(), oxploy_url.clone()));
        }
        trail
    } else if in_git {
        vec![("Git".to_owned(), git_url.clone())]
    } else {
        let mut trail = vec![("Projects".to_owned(), projects_url.clone())];
        trail.extend(projects::trail(cx).await?);
        trail
    };
    let last = trail.len() - 1;

    Ok(view! {
        sidebar_provider(
            sidebar(
                open: $(open.get()),
                mobile_open: $(mobile_open.get()),
                variant: SidebarVariant::Inset,
                collapsible: SidebarCollapsible::Icon,
                sidebar_header(
                    <div class="flex items-center gap-1">
                        organization_switcher(menu: &menu)
                        <div class="md:hidden">
                            button(
                                variant: ButtonVariant::Ghost,
                                size: ButtonSize::Icon,
                                attrs: attributes! {
                                    type="button"
                                    aria-label="Close sidebar"
                                    @click=$(|_e: Event| mobile_open.set(false))
                                },
                                icon(data: iconify_icon!("lucide:x"))
                            )
                        </div>
                    </div>
                )
                sidebar_content(
                    sidebar_group(
                        sidebar_group_label("Workspace")
                        sidebar_menu(
                            sidebar_menu_item(
                                sidebar_menu_button(
                                    href: Some(&projects_url),
                                    active: in_projects,
                                    tooltip: Some("Projects"),
                                    attrs: attributes! { class=(CURRENT_PAGE) },
                                    icon(data: iconify_icon!("lucide:layers"))
                                    <span>"Projects"</span>
                                )
                            )
                            sidebar_menu_item(
                                sidebar_menu_button(
                                    href: Some(&git_url),
                                    active: in_git,
                                    tooltip: Some("Git"),
                                    attrs: attributes! { class=(CURRENT_PAGE) },
                                    icon(data: iconify_icon!("lucide:git-merge"))
                                    <span>"Git"</span>
                                )
                            )
                        )
                    )
                    if owns_server {
                        sidebar_group(
                            sidebar_group_label("Server")
                            sidebar_menu(
                                sidebar_menu_item(
                                    sidebar_menu_button(
                                        href: Some(&monitoring_url),
                                        active: on_server_page,
                                        tooltip: Some("Monitoring"),
                                        attrs: attributes! { class=(CURRENT_PAGE) },
                                        icon(data: iconify_icon!("lucide:activity"))
                                        <span>"Monitoring"</span>
                                    )
                                )
                            )
                        )
                    }
                )
                sidebar_footer(account(user: &signed_in.user))
                sidebar_rail(open: $(open.get()), attrs: attributes! { @click=$(|_e: Event| open.toggle()) })
            )
            sidebar_inset(
                <header class="flex h-14 shrink-0 items-center gap-3 border-b border-border px-4">
                    <div class="hidden md:contents">
                        sidebar_trigger(
                            open: $(open.get()),
                            attrs: attributes! { class="-ml-1" @click=$(|_e: Event| open.toggle()) }
                        )
                    </div>
                    <div class="contents md:hidden">
                        sidebar_trigger(
                            open: $(mobile_open.get()),
                            attrs: attributes! { class="-ml-1" @click=$(|_e: Event| mobile_open.toggle()) }
                        )
                    </div>
                    // A full-height divider between the sidebar toggle and the breadcrumb.
                    separator(orientation: SeparatorOrientation::Vertical)
                    <nav aria-label="Breadcrumb" class="min-w-0 flex-1">
                        <ol class="flex min-w-0 items-center gap-1.5 text-sm">
                            if on_server_page {
                                <li class="flex items-center gap-1.5 text-muted-foreground max-sm:hidden">
                                    icon(data: iconify_icon!("lucide:server"), size: 14)
                                    "Server"
                                </li>
                            } else {
                                <li class="truncate text-muted-foreground max-sm:hidden">(&organization)</li>
                            }
                            // Phones show only the current page.
                            #[key(index)]
                            for (index, (label, url)) in trail.iter().enumerate() {
                                <li aria-hidden="true" class="text-muted-foreground/50 max-sm:hidden">"/"</li>
                                <li class=(if index == last { "min-w-0" } else { "min-w-0 max-sm:hidden" })>
                                    if index == last {
                                        <a class="block truncate font-medium" href=(url) aria-current="page">(label)</a>
                                    } else {
                                        <a class="block truncate text-muted-foreground hover:text-foreground" href=(url)>(label)</a>
                                    }
                                </li>
                            }
                        </ol>
                    </nav>
                    server_time()
                </header>
                <div class="flex w-full flex-1 flex-col p-6">(slot)</div>
            )
        )
        organization_dialogs(menu: &menu)
    })
}

/// The signed-in user and a button to sign out.
#[component]
async fn account(user: &User) -> Result<impl View> {
    let initials: String = [&user.first_name, &user.last_name]
        .iter()
        .filter_map(|name| name.chars().next())
        .flat_map(char::to_uppercase)
        .collect();

    Ok(view! {
        <div class="flex items-center gap-2 rounded-md p-1">
            <span class="flex size-8 shrink-0 items-center justify-center rounded-md bg-sidebar-primary/15 text-xs font-semibold text-sidebar-primary">
                (initials)
            </span>
            <div class="flex min-w-0 flex-1 flex-col md:group-data-[collapsible=icon]/sidebar:hidden">
                <span class="truncate text-sm font-medium">
                    (&user.first_name) " " (&user.last_name)
                </span>
                <span class="truncate text-xs text-muted-foreground">(&user.email)</span>
            </div>
            <form
                method="post"
                action=(href!(crate::app::logout::logout))
                class="md:group-data-[collapsible=icon]/sidebar:hidden"
            >
                button(
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Icon,
                    attrs: attributes! { type="submit" aria-label="Sign out" title="Sign out" },
                    icon(data: iconify_icon!("lucide:log-out"))
                )
            </form>
        </div>
        <p class="px-2 text-[11px] text-muted-foreground md:group-data-[collapsible=icon]/sidebar:hidden">
            "oxploy v" (env!("CARGO_PKG_VERSION"))
        </p>
    })
}

/// The server's clock and time zone. The server renders the time once, and
/// `server-time.js` keeps it ticking in the browser.
#[component]
async fn server_time() -> Result<impl View> {
    let now = jiff::Zoned::now();
    let zone = now.time_zone().iana_name().unwrap_or("UTC").to_owned();
    let offset = now.offset().seconds();

    Ok(view! {
        <div
            class="flex shrink-0 items-center gap-2 rounded-md border border-border bg-muted/60 px-2.5 py-1 text-xs whitespace-nowrap"
            title="Server time"
        >
            icon(
                data: iconify_icon!("lucide:clock"),
                size: 14,
                attrs: attributes! { class="text-muted-foreground" }
            )
            <span class="sr-only">"Server time"</span>
            <time
                class="font-mono font-medium tabular-nums"
                data-server-time=(now.timestamp().as_millisecond().to_string())
                data-utc-offset=(offset.to_string())
            >
                (now.strftime("%H:%M:%S").to_string())
            </time>
            <span class="hidden text-muted-foreground sm:inline">(zone)</span>
            <span class="hidden rounded bg-primary/10 px-1.5 py-0.5 font-mono text-[11px] text-primary sm:inline">
                (utc_offset(offset))
            </span>
        </div>
        <script type="module" src=(asset!("assets/server-time.js"))></script>
    })
}

/// Formats an offset from UTC in seconds as `UTC+00:00`.
fn utc_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.unsigned_abs() / 60;
    format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}
