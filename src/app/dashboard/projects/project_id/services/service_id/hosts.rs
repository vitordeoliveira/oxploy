//! The Domains tab: the addresses that reach the app, through Caddy.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, not_found, see_other},
        href, route,
    },
    runtime::{Event, Signal, connected, shard, signal},
    view::{View, attributes, class, component, element_id, view},
};

use super::super::super::ProjectId;
use super::{ServiceId, ServiceTab, current_service, settings_card};
use crate::app::dashboard::proxy::{caddy_notice, resolution_label};
use crate::app::dashboard::require_organization;
use crate::components::alert_dialog::alert_dialog;
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::dialog::{
    dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title,
};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::components::switch::switch;
use crate::features::caddy::Caddy;
use crate::features::domain::{self, Domain, NewDomain};
use crate::features::project::Project;
use crate::features::service::Service;

/// The service's domains, with whether Caddy serves them.
#[component]
pub async fn domains_tab(cx: &Cx, project: &Project, service: &Service) -> Result<impl View> {
    let domains = Domain::of_service(cx, service.id).await?;
    let caddy = Caddy::get(cx);
    let status = caddy.status();
    let admin = caddy.ports().admin.clone();
    let addresses: Vec<String> = domain::addresses_to_point_at()
        .iter()
        .map(ToString::to_string)
        .collect();
    // The domain the next deployment gives the app, if it would give one.
    let generated = domain::generated_address(&caddy.ports().public_ips)
        .filter(|_| service.domain_offered_at.is_none())
        .map(|address| domain::generated_host(&service.app_name, address));
    let adding = signal(cx, || false);
    let open_adding = adding.clone();
    // Keyed by domain, so each one gets dialogs of its own.
    let dialogs: Vec<_> = domains
        .iter()
        .map(|domain| {
            let cx = cx.keyed(domain.id);
            (signal(&cx, || false), signal(&cx, || false))
        })
        .collect();
    let (project_id, service_id) = (project.id, service.id);
    let add_url = href!(add_domain, ProjectId(project_id), ServiceId(service_id)).resolve(cx);
    let edit_url = href!(edit_domain, ProjectId(project_id), ServiceId(service_id)).resolve(cx);
    let delete_url = href!(delete_domain, ProjectId(project_id), ServiceId(service_id)).resolve(cx);

    Ok(view! {
        <div class="flex flex-col gap-6">
            settings_card(
                title: "Domains",
                text: "The addresses that reach the app. Caddy takes their requests and passes them to it, over HTTPS with certificates it gets and renews.",
                caddy_notice(status: &status, admin: &admin)
                <div class="flex flex-wrap items-start justify-between gap-3">
                    <p class="flex max-w-2xl items-start gap-2 text-sm text-muted-foreground">
                        icon(data: iconify_icon!("lucide:info"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0" })
                        <span>
                            "Point each domain's A or AAAA record to this server"
                            if addresses.is_empty() {
                                "."
                            } else {
                                ": " <span class="font-mono break-all text-foreground">(addresses.join(", "))</span> "."
                            }
                        </span>
                    </p>
                    button(
                        size: ButtonSize::Sm,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open_adding.set(true)) },
                        icon(data: iconify_icon!("lucide:plus"))
                        "Add domain"
                    )
                </div>
                if domains.is_empty() {
                    <p class="rounded-lg border border-dashed border-border p-4 text-sm text-muted-foreground">
                        if let Some(host) = &generated {
                            "No domain yet. The next deployment gives the app "
                            <span class="font-mono break-all text-foreground">(host)</span>
                            ", which points to this server through sslip.io, unless it has a domain by then."
                        } else {
                            "No domain yet. Without one, the app answers only on 127.0.0.1, on this machine."
                        }
                    </p>
                } else {
                    <ul class="divide-y divide-border rounded-lg border border-border">
                        #[key(domain.id)]
                        for (index, domain) in domains.iter().enumerate() {
                            domain_row(
                                project_id: project_id,
                                service_id: service_id,
                                domain: domain,
                                editing: &dialogs[index].0,
                                deleting: &dialogs[index].1
                            )
                        }
                    </ul>
                }
            )
        </div>
        domain_dialog(open: &adding, action: &add_url)
        #[key(domain.id)]
        for (index, domain) in domains.iter().enumerate() {
            domain_dialog(open: &dialogs[index].0, action: &edit_url, domain: Some(domain))
            delete_dialog(open: &dialogs[index].1, domain: domain, action: &delete_url)
        }
    })
}

/// One domain: its address, how it is served, where its DNS points, and its buttons.
#[component]
async fn domain_row(
    project_id: u64,
    service_id: u64,
    domain: &Domain,
    editing: &Signal<bool>,
    deleting: &Signal<bool>,
) -> Result<impl View> {
    let (editing, deleting) = (editing.clone(), deleting.clone());
    let address = format!("{}{}", domain.host, domain.path.as_deref().unwrap_or(""));

    Ok(view! {
        <li class="flex flex-col gap-3 p-4 sm:flex-row sm:items-center sm:justify-between">
            <div class="flex min-w-0 flex-col gap-2">
                <a
                    href=(domain.url())
                    target="_blank"
                    rel="noreferrer"
                    class="flex min-w-0 items-center gap-1.5 font-medium hover:underline"
                >
                    if domain.https {
                        icon(data: iconify_icon!("lucide:lock"), size: 14, attrs: attributes! { class="shrink-0 text-muted-foreground" })
                    } else {
                        icon(data: iconify_icon!("lucide:globe"), size: 14, attrs: attributes! { class="shrink-0 text-muted-foreground" })
                    }
                    <span class="truncate">(&address)</span>
                    icon(data: iconify_icon!("lucide:external-link"), size: 14, attrs: attributes! { class="shrink-0 text-muted-foreground" })
                </a>
                <div class="flex flex-wrap items-center gap-2 text-xs">
                    <span class="rounded-full border border-border px-2 py-0.5 text-muted-foreground">
                        (if domain.https { "HTTPS" } else { "Plain HTTP" })
                    </span>
                    if let (true, Some(path)) = (domain.strip_path, &domain.path) {
                        <span class="rounded-full border border-border px-2 py-0.5 text-muted-foreground">
                            "Strips " (path)
                        </span>
                    }
                    if let Some(www) = domain.www() {
                        <span class="rounded-full border border-border px-2 py-0.5 text-muted-foreground">
                            (www) " redirects here"
                        </span>
                    }
                    dns_status(project_id: project_id, service_id: service_id, host: domain.host.clone())
                    if let Some(www) = domain.www() {
                        dns_status(project_id: project_id, service_id: service_id, host: www)
                    }
                </div>
            </div>
            <div class="flex items-center gap-1 self-end sm:self-auto">
                button(
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Icon,
                    attrs: attributes! {
                        type="button"
                        title="Edit"
                        aria-label=(format!("Edit {address}"))
                        @click=$(|_e: Event| editing.set(true))
                    },
                    icon(data: iconify_icon!("lucide:pencil"))
                )
                button(
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Icon,
                    attrs: attributes! {
                        type="button"
                        class="hover:text-destructive"
                        title="Delete"
                        aria-label=(format!("Delete {address}"))
                        @click=$(|_e: Event| deleting.set(true))
                    },
                    icon(data: iconify_icon!("lucide:trash-2"))
                )
            </div>
        </li>
    })
}

/// Where `host` points, looked up once the page is connected, since a lookup can take
/// seconds. A shard answers requests of its own, so it checks access itself, and looks
/// up only the service's own hosts.
#[shard]
async fn dns_status(cx: &Cx, project_id: u64, service_id: u64, host: String) -> Result<impl View> {
    let organization = require_organization(cx).await?;
    let project = Project::find_in(cx, organization.id, project_id)
        .await?
        .ok_or_not_found()?;
    let service = Service::find_in(cx, project.id, service_id)
        .await?
        .ok_or_not_found()?;
    let domains = Domain::of_service(cx, service.id).await?;
    if !domains
        .iter()
        .any(|domain| domain.host == host || domain.www().as_deref() == Some(host.as_str()))
    {
        return Err(not_found().into());
    }
    let resolution = if connected(cx) {
        Some(domain::resolve(&host).await)
    } else {
        None
    };

    Ok(view! {
        resolution_label(host: &host, resolution: resolution.as_ref())
    })
}

/// The form that adds a domain, or edits `domain`.
#[component]
async fn domain_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    action: &str,
    #[default] domain: Option<&Domain>,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let host_id = element_id(cx);
    let path_id = element_id(cx);

    Ok(view! {
        dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(
                        attrs: attributes! { id=(&title_id) },
                        (if domain.is_some() { "Edit domain" } else { "Add domain" })
                    )
                    dialog_description("Caddy sends the domain's requests to the app as soon as it is saved.")
                )
                // A group, so the options below show only where they apply: stripping
                // with a path, redirecting www without one.
                <form method="post" action=(action) class="group flex flex-col gap-4">
                    if let Some(domain) = domain {
                        <input type="hidden" name="id" value=(domain.id.to_string())>
                    }
                    field(
                        field_label(attrs: attributes! { for=(&host_id) }, "Domain")
                        input(
                            attrs: attributes! {
                                id=(&host_id)
                                name="host"
                                value=(domain.map_or("", |domain| domain.host.as_str()))
                                placeholder="shop.example.com"
                                maxlength="253"
                                autocomplete="off"
                                autocapitalize="off"
                                spellcheck="false"
                                required=""
                            }
                        )
                    )
                    field(
                        field_label(attrs: attributes! { for=(&path_id) }, "Path")
                        input(
                            attrs: attributes! {
                                id=(&path_id)
                                name="path"
                                class="domain-path"
                                value=(domain.and_then(|domain| domain.path.as_deref()).unwrap_or(""))
                                placeholder="/"
                                maxlength="255"
                                autocomplete="off"
                                autocapitalize="off"
                                spellcheck="false"
                            }
                        )
                        field_description("Optional. Only the requests under it go to the app, such as /api.")
                    )
                    option_switch(
                        name: "strip_path",
                        checked: domain.is_some_and(|domain| domain.strip_path),
                        label: "Strip the path",
                        text: "The app gets /api/orders as /orders.",
                        class: "group-has-[.domain-path:placeholder-shown]:hidden"
                    )
                    option_switch(
                        name: "https",
                        checked: domain.is_none_or(|domain| domain.https),
                        label: "HTTPS",
                        text: "Caddy gets a certificate from Let's Encrypt and renews it. The domain must point to this server, with ports 80 and 443 open."
                    )
                    option_switch(
                        name: "redirect_www",
                        checked: domain.is_some_and(|domain| domain.redirect_www),
                        label: "Redirect www",
                        text: "www. and the domain sends its visitors here. Its DNS must point to this server too.",
                        class: "group-has-[.domain-path:not(:placeholder-shown)]:hidden"
                    )
                    dialog_footer(
                        button(
                            variant: ButtonVariant::Ghost,
                            attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                            "Cancel"
                        )
                        button(
                            attrs: attributes! { type="submit" },
                            (if domain.is_some() { "Save domain" } else { "Add domain" })
                        )
                    )
                </form>
            )
        )
    })
}

/// A setting of a form, switched on or off.
#[component]
pub(super) async fn option_switch(
    cx: &Cx,
    name: &'static str,
    checked: bool,
    label: &'static str,
    text: &'static str,
    #[default] class: &'static str,
) -> Result<impl View> {
    let id = element_id(cx);

    Ok(view! {
        <div class=(class!("flex items-start justify-between gap-4 rounded-lg border border-border p-3", class))>
            <div class="flex flex-col gap-0.5">
                <label for=(&id) class="text-sm font-medium">(label)</label>
                <p class="text-xs text-muted-foreground">(text)</p>
            </div>
            switch(attrs: attributes! { id=(&id) name=(name) class="mt-0.5" checked=(checked.then_some("")) })
        </div>
    })
}

/// A dialog that confirms deleting a domain.
#[component]
async fn delete_dialog(cx: &Cx, open: &Signal<bool>, domain: &Domain, action: &str) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);

    Ok(view! {
        alert_dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Delete " (&domain.host) (domain.path.as_deref().unwrap_or("")))
                    dialog_description("Caddy stops sending its requests to the app. The DNS record stays as it is.")
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(action)>
                        <input type="hidden" name="id" value=(domain.id.to_string())>
                        button(
                            variant: ButtonVariant::Destructive,
                            attrs: attributes! { type="submit" },
                            "Delete domain"
                        )
                    </form>
                )
            )
        )
    })
}

/// The domain form. A switch sends a value only while it is on.
#[derive(Deserialize)]
struct DomainForm {
    #[serde(default)]
    id: Option<u64>,
    host: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    strip_path: Option<String>,
    #[serde(default)]
    https: Option<String>,
    #[serde(default)]
    redirect_www: Option<String>,
}

impl DomainForm {
    fn domain(&self) -> NewDomain<'_> {
        NewDomain {
            host: &self.host,
            path: &self.path,
            strip_path: self.strip_path.is_some(),
            https: self.https.is_some(),
            redirect_www: self.redirect_www.is_some(),
        }
    }
}

/// Adds a domain to the service.
#[route(POST "./add-domain")]
async fn add_domain(cx: &Cx, Form(form): Form<DomainForm>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    if let Err(problem) = Domain::add(cx, &service, &form.domain()).await? {
        return Err(bad_request(problem).into());
    }
    Caddy::get(cx).changed();
    Ok(see_other(ServiceTab::Domains.url(cx, &project, &service)))
}

/// Changes one of the service's domains.
#[route(POST "./edit-domain")]
async fn edit_domain(cx: &Cx, Form(form): Form<DomainForm>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    let id = form.id.ok_or_else(|| bad_request("No domain was chosen"))?;
    let mut domain = Domain::find_of(cx, service.id, id).await?.ok_or_not_found()?;
    if let Err(problem) = domain.edit(cx, &form.domain()).await? {
        return Err(bad_request(problem).into());
    }
    Caddy::get(cx).changed();
    Ok(see_other(ServiceTab::Domains.url(cx, &project, &service)))
}

/// A form that names a domain.
#[derive(Deserialize)]
struct ChosenDomain {
    id: u64,
}

/// Deletes one of the service's domains.
#[route(POST "./delete-domain")]
async fn delete_domain(cx: &Cx, Form(form): Form<ChosenDomain>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    let domain = Domain::find_of(cx, service.id, form.id).await?.ok_or_not_found()?;
    domain.remove(cx).await?;
    Caddy::get(cx).changed();
    Ok(see_other(ServiceTab::Domains.url(cx, &project, &service)))
}
