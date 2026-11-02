//! The organization switcher at the top of the sidebar, and the routes behind it.
//!
//! Each change is a form posted to a route here, which then sends the browser back to
//! the page it was on.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        href, route,
    },
    runtime::{Event, Signal, signal},
    view::{View, attributes, class, component, element_id, view},
};

use super::require_sign_in;
use crate::components::alert_dialog::alert_dialog;
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::dialog::{
    dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title,
};
use crate::components::field::{field, field_label};
use crate::components::floating::{FloatingAlign, FloatingSide};
use crate::components::input::input;
use crate::components::popover::{popover, popover_content, popover_trigger};
use crate::components::sidebar::{
    SidebarMenuButtonSize, SidebarMenuButtonVariant, sidebar_menu, sidebar_menu_button_variants,
    sidebar_menu_item,
};
use crate::features::organization::{
    MAX_LOGO_URL_LEN, MAX_NAME_LEN, Organization, OrganizationDetails,
};
use crate::features::session::{Organizations, SignedIn};

/// The user's organizations as the sidebar shows them, with the browser state of the
/// dialogs that change them.
pub struct OrganizationMenu {
    organizations: Organizations,
    /// Whether the dialog that edits each organization is open, in the same order.
    editing: Vec<Signal<bool>>,
    /// Whether the dialog that deletes each organization is open, in the same order.
    deleting: Vec<Signal<bool>>,
    /// Whether the dialog that adds an organization is open.
    adding: Signal<bool>,
    /// The ID of the switcher's popover, which its buttons close.
    popover: String,
    /// The page the forms send the browser back to.
    return_to: String,
}

impl OrganizationMenu {
    pub fn new(cx: &Cx, organizations: Organizations, return_to: String) -> Self {
        // Keyed by organization, so each one gets signals of its own.
        let editing = organizations
            .all
            .iter()
            .map(|organization| signal(&cx.keyed(organization.id), || false))
            .collect();
        let deleting = organizations
            .all
            .iter()
            .map(|organization| signal(&cx.keyed(organization.id), || false))
            .collect();
        Self {
            editing,
            deleting,
            organizations,
            adding: signal(cx, || false),
            popover: element_id(cx),
            return_to,
        }
    }
}

/// The active organization, which opens a list of all of them to switch, edit, delete
/// or add one.
#[component]
pub async fn organization_switcher(menu: &OrganizationMenu) -> Result<impl View> {
    let Organizations {
        all,
        active,
        default,
    } = &menu.organizations;
    let current = &all[*active];
    // A user always keeps at least one organization.
    let can_delete = all.len() > 1;
    let adding = menu.adding.clone();
    let count = match all.len() {
        1 => "1 organization".to_owned(),
        count => format!("{count} organizations"),
    };

    Ok(view! {
        sidebar_menu(
            attrs: attributes! { class="min-w-0 flex-1" },
            sidebar_menu_item(
                popover(
                    popover_trigger(
                        target: &menu.popover,
                        attrs: attributes! {
                            class=(class!(
                                sidebar_menu_button_variants(
                                    SidebarMenuButtonVariant::Default,
                                    SidebarMenuButtonSize::Lg,
                                ),
                                "group-has-[:popover-open]/popover:bg-sidebar-accent",
                            ))
                            title="Switch organization"
                        },
                        organization_logo(organization: current, large: true)
                        <span class="flex min-w-0 flex-1 flex-col leading-tight md:group-data-[collapsible=icon]/sidebar:hidden">
                            <span class="truncate font-medium">(&current.name)</span>
                            <span class="truncate text-xs text-muted-foreground">(count)</span>
                        </span>
                        icon(
                            data: iconify_icon!("lucide:chevrons-up-down"),
                            attrs: attributes! { class="shrink-0 text-muted-foreground md:group-data-[collapsible=icon]/sidebar:hidden" }
                        )
                    )
                    popover_content(
                        id: &menu.popover,
                        side: FloatingSide::Bottom,
                        align: FloatingAlign::Start,
                        attrs: attributes! { class="[&]:w-72 [&]:p-1.5" },
                        <div class="px-2 pt-1 pb-2 text-xs font-medium text-muted-foreground">
                            "Organizations"
                        </div>
                        #[key(organization.id)]
                        for (index, organization) in all.iter().enumerate() {
                            let editing = menu.editing[index].clone();
                            let deleting = menu.deleting[index].clone();
                            let is_active = index == *active;
                            let is_default = index == *default;
                            <div class="group/organization flex items-center gap-0.5 rounded-md pr-1 hover:bg-foreground/5">
                                <form method="post" action=(href!(activate)) class="min-w-0 flex-1">
                                    hidden_fields(id: organization.id, return_to: &menu.return_to)
                                    <button
                                        type="submit"
                                        class="flex w-full min-w-0 cursor-pointer items-center gap-2 rounded-md p-1.5 text-left text-sm"
                                        aria-current=(if is_active { "true" } else { "false" })
                                    >
                                        organization_logo(organization: organization)
                                        <span class="truncate">(&organization.name)</span>
                                        if is_active {
                                            icon(
                                                data: iconify_icon!("lucide:check"),
                                                size: 14,
                                                attrs: attributes! { class="shrink-0 text-primary" }
                                            )
                                        }
                                    </button>
                                </form>
                                if is_default {
                                    <span
                                        class="flex size-7 shrink-0 items-center justify-center text-primary"
                                        title="Default organization"
                                        data-default=""
                                    >
                                        icon(
                                            data: iconify_icon!("lucide:star"),
                                            size: 14,
                                            label: "Default organization",
                                            attrs: attributes! { class="[&_path]:fill-current" }
                                        )
                                    </span>
                                }
                                // The other actions show when the row is hovered or focused.
                                <div class="flex items-center md:opacity-0 md:group-focus-within/organization:opacity-100 md:group-hover/organization:opacity-100">
                                    if !is_default {
                                        <form method="post" action=(href!(make_default))>
                                            hidden_fields(id: organization.id, return_to: &menu.return_to)
                                            button(
                                                variant: ButtonVariant::Ghost,
                                                size: ButtonSize::Icon,
                                                attrs: attributes! {
                                                    type="submit"
                                                    class="size-7 text-muted-foreground hover:text-primary"
                                                    title="Set as default"
                                                    aria-label="Set as default"
                                                },
                                                icon(data: iconify_icon!("lucide:star"), size: 14)
                                            )
                                        </form>
                                    }
                                    button(
                                        variant: ButtonVariant::Ghost,
                                        size: ButtonSize::Icon,
                                        attrs: attributes! {
                                            type="button"
                                            class="size-7 text-muted-foreground hover:text-foreground"
                                            title="Edit organization"
                                            aria-label="Edit organization"
                                            popovertarget=(&menu.popover)
                                            popovertargetaction="hide"
                                            @click=$(|_e: Event| editing.set(true))
                                        },
                                        icon(data: iconify_icon!("lucide:pencil"), size: 14)
                                    )
                                    if can_delete {
                                        button(
                                            variant: ButtonVariant::Ghost,
                                            size: ButtonSize::Icon,
                                            attrs: attributes! {
                                                type="button"
                                                class="size-7 text-muted-foreground hover:text-destructive"
                                                title="Delete organization"
                                                aria-label="Delete organization"
                                                popovertarget=(&menu.popover)
                                                popovertargetaction="hide"
                                                @click=$(|_e: Event| deleting.set(true))
                                            },
                                            icon(data: iconify_icon!("lucide:trash-2"), size: 14)
                                        )
                                    }
                                </div>
                            </div>
                        }
                        <div class="-mx-1.5 my-1.5 h-px bg-border"></div>
                        <button
                            type="button"
                            class="flex w-full cursor-pointer items-center gap-2 rounded-md p-1.5 text-sm text-muted-foreground hover:bg-foreground/5 hover:text-foreground"
                            popovertarget=(&menu.popover)
                            popovertargetaction="hide"
                            @click=$(|_e: Event| adding.set(true))
                        >
                            <span class="flex size-6 items-center justify-center rounded-md border border-dashed border-border">
                                icon(data: iconify_icon!("lucide:plus"), size: 14)
                            </span>
                            "New organization"
                        </button>
                    )
                )
            )
        )
    })
}

/// The dialogs the switcher opens. They render outside the sidebar, whose panel would
/// otherwise confine their overlay.
#[component]
pub async fn organization_dialogs(cx: &Cx, menu: &OrganizationMenu) -> Result<impl View> {
    Ok(view! {
        details_dialog(
            open: &menu.adding,
            title: "New organization",
            description: "Organizations keep separate sets of projects.",
            action: href!(create).resolve(cx),
            organization: None,
            submit: "Create organization",
            return_to: &menu.return_to
        )
        #[key(organization.id)]
        for (index, organization) in menu.organizations.all.iter().enumerate() {
            details_dialog(
                open: &menu.editing[index],
                title: "Edit organization",
                description: "Change the name and logo of this organization.",
                action: href!(edit).resolve(cx),
                organization: Some(organization),
                submit: "Save changes",
                return_to: &menu.return_to
            )
            delete_dialog(
                open: &menu.deleting[index],
                organization: organization,
                return_to: &menu.return_to
            )
        }
    })
}

/// The organization's logo or, when it has none, its initial on a rust tile. `large`
/// sizes it for the sidebar.
#[component]
async fn organization_logo(
    organization: &Organization,
    #[default] large: bool,
) -> Result<impl View> {
    let initial: String = organization
        .name
        .chars()
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .collect();

    Ok(view! {
        <span
            class=(class!(
                "flex shrink-0 items-center justify-center overflow-hidden rounded-md",
                if large { "size-8 text-sm" } else { "size-6 text-xs" },
            ))
        >
            if let Some(logo_url) = &organization.logo_url {
                <img
                    src=(logo_url)
                    alt=""
                    class="size-full object-cover"
                    referrerpolicy="no-referrer"
                >
            } else {
                <span class="flex size-full items-center justify-center bg-primary font-semibold text-primary-foreground">
                    (initial)
                </span>
            }
        </span>
    })
}

/// The fields every organization form carries: which organization, and where to go
/// back to.
#[component]
async fn hidden_fields(id: u64, return_to: &str) -> Result<impl View> {
    Ok(view! {
        <input type="hidden" name="id" value=(id.to_string())>
        <input type="hidden" name="return_to" value=(return_to)>
    })
}

/// A dialog with the name and logo of a new organization, or of `organization`.
#[component]
async fn details_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    title: &'static str,
    description: &'static str,
    action: String,
    organization: Option<&Organization>,
    submit: &'static str,
    return_to: &str,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let name_id = element_id(cx);
    let logo_url_id = element_id(cx);
    let name = organization.map_or("", |organization| organization.name.as_str());
    let logo_url = organization
        .and_then(|organization| organization.logo_url.as_deref())
        .unwrap_or("");

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
                    <input type="hidden" name="return_to" value=(return_to)>
                    if let Some(organization) = organization {
                        <input type="hidden" name="id" value=(organization.id.to_string())>
                    }
                    field(
                        field_label(attrs: attributes! { for=(&name_id) }, "Name")
                        input(
                            attrs: attributes! {
                                id=(&name_id)
                                name="name"
                                value=(name)
                                placeholder="Organization name"
                                maxlength=(MAX_NAME_LEN.to_string())
                                required=""
                            }
                        )
                    )
                    field(
                        field_label(attrs: attributes! { for=(&logo_url_id) }, "Logo URL")
                        input(
                            attrs: attributes! {
                                id=(&logo_url_id)
                                type="url"
                                name="logo_url"
                                value=(logo_url)
                                placeholder="https://example.com/logo.png"
                                pattern="https?://.+"
                                maxlength=(MAX_LOGO_URL_LEN.to_string())
                            }
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

/// A dialog that confirms deleting `organization`.
#[component]
async fn delete_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    organization: &Organization,
    return_to: &str,
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
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Delete organization")
                    dialog_description(
                        attrs: attributes! { id=(&description_id) },
                        "Are you sure you want to delete "
                        <strong class="font-medium text-foreground">(&organization.name)</strong>
                        "? This cannot be undone."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(href!(remove))>
                        hidden_fields(id: organization.id, return_to: return_to)
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

/// A form that adds an organization.
#[derive(Deserialize)]
struct NewOrganizationForm {
    name: String,
    #[serde(default)]
    logo_url: String,
    return_to: String,
}

/// A form that changes an organization's details.
#[derive(Deserialize)]
struct EditOrganizationForm {
    id: u64,
    name: String,
    #[serde(default)]
    logo_url: String,
    return_to: String,
}

/// A form that acts on an organization.
#[derive(Deserialize)]
struct OrganizationForm {
    id: u64,
    return_to: String,
}

/// The response to details the form's own validation would have rejected.
const INVALID_DETAILS: &str = "The name is required, and the logo must be an http or https URL";

/// Adds an organization for the signed-in user.
#[route(POST)]
async fn create(cx: &Cx, Form(form): Form<NewOrganizationForm>) -> Result<SeeOther> {
    let signed_in = require_sign_in(cx).await?;
    let details = OrganizationDetails {
        name: &form.name,
        logo_url: &form.logo_url,
    };
    if Organization::add(cx, &signed_in.user, &details)
        .await?
        .is_none()
    {
        return Err(bad_request(INVALID_DETAILS).into());
    }
    Ok(back(cx, &form.return_to))
}

/// Changes the name and logo of one of the user's organizations.
#[route(POST "./edit")]
async fn edit(cx: &Cx, Form(form): Form<EditOrganizationForm>) -> Result<SeeOther> {
    let signed_in = require_sign_in(cx).await?;
    let mut organization = owned(cx, &signed_in, form.id).await?;
    let details = OrganizationDetails {
        name: &form.name,
        logo_url: &form.logo_url,
    };
    if !organization.edit(cx, &details).await? {
        return Err(bad_request(INVALID_DETAILS).into());
    }
    Ok(back(cx, &form.return_to))
}

/// Deletes one of the user's organizations, unless it is their last.
#[route(POST "./delete")]
async fn remove(cx: &Cx, Form(form): Form<OrganizationForm>) -> Result<SeeOther> {
    let mut signed_in = require_sign_in(cx).await?;
    let organization = owned(cx, &signed_in, form.id).await?;
    if !organization.remove(cx, &mut signed_in.user).await? {
        return Err(bad_request("The last organization cannot be deleted").into());
    }
    Ok(back(cx, &form.return_to))
}

/// Makes one of the user's organizations the one they open after signing in.
#[route(POST "./default")]
async fn make_default(cx: &Cx, Form(form): Form<OrganizationForm>) -> Result<SeeOther> {
    let mut signed_in = require_sign_in(cx).await?;
    let organization = owned(cx, &signed_in, form.id).await?;
    signed_in
        .user
        .set_default_organization(cx, organization.id)
        .await?;
    Ok(back(cx, &form.return_to))
}

/// Switches this browser to one of the user's organizations.
#[route(POST "./activate")]
async fn activate(cx: &Cx, Form(form): Form<OrganizationForm>) -> Result<SeeOther> {
    let mut signed_in = require_sign_in(cx).await?;
    let organization = owned(cx, &signed_in, form.id).await?;
    signed_in.activate(cx, &organization).await?;
    Ok(back(cx, &form.return_to))
}

/// Returns the organization with this ID, or a not found error unless the signed-in
/// user owns it.
async fn owned(cx: &Cx, signed_in: &SignedIn, id: u64) -> Result<Organization> {
    let organization = Organization::find_owned(cx, &signed_in.user, id)
        .await?
        .ok_or_not_found()?;
    Ok(organization)
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
        see_other(href!(super::projects::page).resolve(cx))
    }
}
