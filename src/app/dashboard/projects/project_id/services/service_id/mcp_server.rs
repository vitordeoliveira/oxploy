//! The MCP tab: the MCP server the app serves, and which of its tools the user turned
//! on. See [`app_mcp`].

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
    runtime::{Event, Signal, procedure, signal},
    view::{View, attributes, component, element_id, view},
};

use super::super::super::ProjectId;
use super::hosts::option_switch;
use super::{ServiceId, ServiceTab, current_service, settings_card};
use crate::app::dashboard::mcp::{EFFECT_GROUPS, effect_heading, group_key};
use crate::app::dashboard::require_organization;
use crate::components::button::button;
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::components::switch::switch;
use crate::db::db;
use crate::features::app_mcp::{
    self, ConnectError, DEFAULT_PATH, Effect, Listing, MAX_PATH_LEN, McpTool, Tool,
};
use crate::features::deploy::{AppStatus, Deployer};
use crate::features::environment::MCP_TOKEN_VARIABLE;
use crate::features::project::Project;
use crate::features::service::Service;

/// Whether the app serves MCP and where, and its tools while it does. The app's server
/// is asked once, for the summary at the top and for the tools.
#[component]
pub async fn mcp_tab(cx: &Cx, project: &Project, service: &Service) -> Result<impl View> {
    let server_url = href!(save_server, ProjectId(project.id), ServiceId(service.id)).resolve(cx);
    let path_id = element_id(cx);
    let on = service.mcp_path.is_some();
    let path = service.mcp_path.as_deref().unwrap_or(DEFAULT_PATH);
    let running = Deployer::get(cx).status(service) == AppStatus::Running;
    let listing = match (running, service.port, service.mcp_token_in_use()) {
        (true, Some(port), Some(token)) => Some(app_mcp::list_tools(port, path, token).await),
        _ => None,
    };
    let enabled = McpTool::enabled_of(cx, service.id).await?;
    let (dot, status) = match (on, &listing) {
        (false, _) => ("bg-muted-foreground/60", "Off".to_owned()),
        (true, None) => ("bg-muted-foreground/60", "On, but the app is not running".to_owned()),
        (true, Some(Ok(listing))) => {
            let name = listing.server.title.as_deref().unwrap_or(&listing.server.name);
            let version = listing.server.version.as_deref().unwrap_or_default();
            let protocol = &listing.protocol_version;
            let connected = format!("Connected to {name} {version}");
            ("bg-emerald-500", format!("{} · protocol {protocol}", connected.trim_end()))
        }
        (true, Some(Err(problem))) => ("bg-destructive", sentence(&problem.to_string())),
    };
    let address = match service.port {
        Some(port) => format!("http://127.0.0.1:{port}{path}"),
        None => format!("{path}, on the port chosen at the first deploy"),
    };
    // How many tools are on, which the switches below change without a reload.
    let (initially_on, offered) = match &listing {
        Some(Ok(listing)) => (
            listing.tools.iter().filter(|tool| enabled.contains(&tool.name)).count(),
            Some(listing.tools.len()),
        ),
        _ => (enabled.len(), None),
    };
    let tools_on = signal(cx, || initially_on);

    Ok(view! {
        <div class="flex flex-col gap-6">
            settings_card(
                title: "MCP server",
                text: "An MCP server the app serves on its own port, over Streamable HTTP. oxploy calls it at 127.0.0.1, and the app's domains answer 404 at its path, so only oxploy reaches it.",
                <dl class="grid grid-cols-1 gap-x-6 gap-y-3 text-sm sm:grid-cols-[8rem_minmax(0,1fr)]">
                    <dt class="font-medium">"Status"</dt>
                    <dd class="flex items-center gap-2 text-muted-foreground">
                        <span class=(format!("size-2 shrink-0 rounded-full {dot}"))></span>
                        (status)
                    </dd>
                    if on {
                        <dt class="font-medium">"Address"</dt>
                        <dd class="font-mono break-all text-muted-foreground">(address)</dd>
                    }
                    <dt class="font-medium">"Variable"</dt>
                    <dd class="font-mono text-muted-foreground">(MCP_TOKEN_VARIABLE)</dd>
                    if on {
                        <dt class="font-medium">"Tools"</dt>
                        <dd class="text-muted-foreground">
                            $(tools_on.get())
                            if let Some(offered) = offered {
                                " of " (offered.to_string())
                            }
                            " on"
                        </dd>
                        <dt class="font-medium">"Named"</dt>
                        <dd class="text-muted-foreground">
                            <span class="font-mono break-all">(&service.app_name) "__<tool>"</span>
                            ", in this organization's MCP"
                        </dd>
                    }
                </dl>
            )
            settings_card(
                title: "Settings",
                text: "Whether the app serves MCP, and at which path on its port.",
                <form method="post" action=(&server_url) class="flex flex-col gap-4">
                    option_switch(
                        name: "on",
                        checked: on,
                        label: "This app serves MCP",
                        text: "oxploy lists its tools here, and gives the app the token it calls it with in OXPLOY_MCP_TOKEN."
                    )
                    field(
                        field_label(attrs: attributes! { for=(&path_id) }, "Path")
                        input(
                            attrs: attributes! {
                                id=(&path_id)
                                name="path"
                                value=(path)
                                placeholder=(DEFAULT_PATH)
                                maxlength=(MAX_PATH_LEN.to_string())
                                pattern="(\\/[A-Za-z0-9_.~\\-]+)+"
                                title="A path below the root, such as /mcp"
                                class="font-mono"
                                spellcheck="false"
                                autocomplete="off"
                                autocapitalize="off"
                            }
                        )
                        field_description("Where the app answers MCP on its port, such as /mcp or /api/mcp.")
                    )
                    <div class="flex justify-end">
                        button(attrs: attributes! { type="submit" }, "Save")
                    </div>
                </form>
                if on {
                    <div class="flex flex-col gap-2 rounded-lg bg-muted px-4 py-3 text-sm">
                        <p>
                            "The app gets the token in "
                            <code class="font-mono">(MCP_TOKEN_VARIABLE)</code>
                            " when it starts, so restart it after turning MCP on. It should answer only the requests that send it as "
                            <code class="font-mono">"Authorization: Bearer <token>"</code>
                            "."
                        </p>
                        <p class="text-muted-foreground">
                            "An app that reads another variable, such as MCP_TOKEN, takes it with this line in the Environment tab:"
                        </p>
                        <code class="w-fit rounded-md border border-border bg-background px-2 py-1 font-mono text-xs select-all">
                            "MCP_TOKEN=${OXPLOY_MCP_TOKEN}"
                        </code>
                    </div>
                }
            )
            if on {
                tools_card(service: service, listing: listing.as_ref(), enabled: &enabled, tools_on: &tools_on)
            }
        </div>
    })
}

/// The tools the app's MCP server lists now, each with a switch that turns it on or
/// off, or why they could not be listed.
#[component]
async fn tools_card(
    service: &Service,
    listing: Option<&Result<Listing, ConnectError>>,
    enabled: &[String],
    tools_on: &Signal<usize>,
) -> Result<impl View> {
    let path = service.mcp_path.as_deref().unwrap_or(DEFAULT_PATH);
    let groups = match listing {
        Some(Ok(listing)) => by_effect(&listing.tools),
        _ => Vec::new(),
    };

    Ok(view! {
        settings_card(
            title: "Tools",
            text: "The tools the app's MCP server offers now. The ones you turn on reach this organization's MCP, through the tokens that reach every tool turned on or that have the tool picked. A tool the app starts offering stays off until you turn it on, and turning one off takes it from every token. Clients already connected see a change once they reconnect.",
            match listing {
                None => {
                    <p class="text-sm text-muted-foreground">"The app is not running. Start it to see its tools."</p>
                }
                Some(Err(problem)) => {
                    <div class="flex items-start gap-2 text-sm">
                        icon(data: iconify_icon!("lucide:circle-alert"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0 text-destructive" })
                        <p>(problem_text(problem, path))</p>
                    </div>
                }
                Some(Ok(listing)) => {
                    if listing.tools.is_empty() {
                        <p class="text-sm text-muted-foreground">"Its MCP server offers no tools."</p>
                    } else {
                        <div class="flex flex-col gap-5">
                            #[key(group_key(Some(*effect)))]
                            for (effect, tools) in &groups {
                                <div class="flex flex-col gap-1.5">
                                    effect_heading(effect: Some(*effect), count: tools.len())
                                    <ul class="flex flex-col divide-y divide-border rounded-lg border border-border">
                                        #[key(tool.name.clone())]
                                        for tool in tools {
                                            tool_row(
                                                service_id: service.id,
                                                tool: tool,
                                                on: enabled.contains(&tool.name),
                                                tools_on: tools_on
                                            )
                                        }
                                    </ul>
                                </div>
                            }
                        </div>
                    }
                }
            }
        )
    })
}

/// The tools in the groups of [`EFFECT_GROUPS`] they belong to, leaving out the empty
/// ones. A listed tool always says what it does, so none is in the group of unknowns.
fn by_effect(tools: &[Tool]) -> Vec<(Effect, Vec<&Tool>)> {
    EFFECT_GROUPS
        .into_iter()
        .flatten()
        .map(|effect| (effect, tools.iter().filter(|tool| tool.effect == effect).collect::<Vec<_>>()))
        .filter(|(_, tools)| !tools.is_empty())
        .collect()
}

/// How a tool's row looks while it is on: a bar along its left edge, as the sidebar
/// marks the open page, over a tinted background.
const ROW_ON: &str = "relative flex flex-wrap items-center gap-3 bg-primary/5 px-4 py-3 transition-colors before:absolute before:inset-y-2 before:left-0 before:w-0.75 before:rounded-full before:bg-primary";

/// How a tool's row looks while it is off.
const ROW_OFF: &str = "relative flex flex-wrap items-center gap-3 px-4 py-3 transition-colors";

/// One tool: its name, what it does, and a switch that turns it on or off without
/// opening the page again. Its look, and the count at the top of the
/// tab, follow the switch.
#[component]
async fn tool_row(
    cx: &Cx,
    service_id: u64,
    tool: &Tool,
    on: bool,
    tools_on: &Signal<usize>,
) -> Result<impl View> {
    let about = tool.description.as_deref().or(tool.title.as_deref());
    let turned_on = signal(&cx.keyed(tool.name.clone()), || on);
    let tools_on = tools_on.clone();
    let name = tool.name.clone();
    let switch_id = element_id(cx);

    Ok(view! {
        <li :class=$(if turned_on.get() { ROW_ON } else { ROW_OFF })>
            <div class="flex min-w-56 flex-1 flex-col gap-1">
                <div class="flex flex-wrap items-center gap-2">
                    <label
                        for=(&switch_id)
                        :class=$(if turned_on.get() {
                            "cursor-pointer font-mono text-sm font-semibold break-all text-foreground"
                        } else {
                            "cursor-pointer font-mono text-sm font-medium break-all text-muted-foreground"
                        })
                    >
                        (&tool.name)
                    </label>
                </div>
                if let Some(about) = about {
                    <p class="line-clamp-2 text-sm text-muted-foreground">(about)</p>
                }
            </div>
            <div class="flex items-center gap-2">
                <span
                    :class=$(if turned_on.get() {
                        "w-6 text-right text-xs font-semibold text-primary"
                    } else {
                        "w-6 text-right text-xs text-muted-foreground"
                    })
                >
                    $(if turned_on.get() { "On" } else { "Off" })
                </span>
                switch(
                    attrs: attributes! {
                        id=(&switch_id)
                        :checked=$(turned_on.get())
                        @change=$(async |_e: Event| {
                            let saved = turn_tool(service_id, name, !turned_on.get()).await;
                            turned_on.set(saved);
                            if saved {
                                tools_on.increment()
                            } else {
                                tools_on.decrement()
                            }
                        })
                    }
                )
            </div>
        </li>
    })
}

/// Turns one of an app's tools on or off, from the switch in its row, and returns
/// whether it is on now. It answers requests of its own, so it checks that the service
/// is in the organization the browser works in.
#[procedure]
async fn turn_tool(cx: &Cx, service_id: u64, name: String, on: bool) -> Result<bool> {
    let organization = require_organization(cx).await?;
    let service = Service::filter_by_id(service_id)
        .first()
        .exec(&mut db(cx))
        .await?
        .ok_or_not_found()?;
    Project::find_in(cx, organization.id, service.project_id)
        .await?
        .ok_or_not_found()?;
    if !McpTool::set(cx, service.id, &name, on).await? {
        return Err(bad_request("That is not a tool's name").into());
    }
    Ok(on)
}

/// Why the tools could not be listed, with what to check.
fn problem_text(problem: &ConnectError, path: &str) -> String {
    match problem {
        ConnectError::Refused(status) => format!(
            "The app turned oxploy away with {status}. Restart it, so it gets {MCP_TOKEN_VARIABLE}, and check that it takes the token from there."
        ),
        ConnectError::NotFound => format!(
            "Nothing answers MCP at {path} on the app's port. Check the path, and that the app serves MCP there."
        ),
        problem => format!("{}.", sentence(&problem.to_string())),
    }
}

/// `text` with its first letter in upper case, to start a sentence.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The form that turns the app's MCP server on or off.
#[derive(Deserialize)]
struct ServerForm {
    /// Sent, as `on`, only while the switch is on.
    on: Option<String>,
    #[serde(default)]
    path: String,
}

/// Turns the app's MCP server on at the path, or off.
#[route(POST "./settings")]
async fn save_server(cx: &Cx, Form(form): Form<ServerForm>) -> Result<SeeOther> {
    let (project, mut service) = current_service(cx).await?;
    let path = form.on.is_some().then_some(form.path.as_str());
    if !service.set_mcp(cx, path).await? {
        return Err(bad_request(
            "The path starts with / and has letters, digits, -, _, . or ~, such as /mcp",
        )
        .into());
    }
    Ok(see_other(ServiceTab::Mcp.url(cx, &project, &service)))
}

