//! The MCP tab: the MCP server the app serves, and which of its tools the user turned
//! on. See [`app_mcp`].

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
use super::hosts::option_switch;
use super::{ServiceId, ServiceTab, current_service, settings_card};
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
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
    let tools = match &listing {
        Some(Ok(listing)) => {
            let on = listing.tools.iter().filter(|tool| enabled.contains(&tool.name)).count();
            format!("{on} of {} on", listing.tools.len())
        }
        _ => format!("{} on", enabled.len()),
    };

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
                        <dd class="text-muted-foreground">(tools)</dd>
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
                tools_card(project: project, service: service, listing: listing.as_ref(), enabled: &enabled)
            }
        </div>
    })
}

/// The tools the app's MCP server lists now, each with a button that turns it on or
/// off, or why they could not be listed.
#[component]
async fn tools_card(
    cx: &Cx,
    project: &Project,
    service: &Service,
    listing: Option<&Result<Listing, ConnectError>>,
    enabled: &[String],
) -> Result<impl View> {
    let path = service.mcp_path.as_deref().unwrap_or(DEFAULT_PATH);
    let toggle_url = href!(toggle_tool, ProjectId(project.id), ServiceId(service.id)).resolve(cx);

    Ok(view! {
        settings_card(
            title: "Tools",
            text: "The tools the app's MCP server offers now. The ones you turn on reach this organization's MCP, through its tokens, and a tool the app starts offering stays off until you turn it on. Clients already connected see a change once they reconnect.",
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
                        <ul class="flex flex-col divide-y divide-border rounded-lg border border-border">
                            #[key(tool.name.clone())]
                            for tool in &listing.tools {
                                tool_row(tool: tool, on: enabled.contains(&tool.name), action: &toggle_url)
                            }
                        </ul>
                    }
                }
            }
        )
    })
}

/// One tool: its name, what it does to the app's data, and a button that turns it on or
/// off.
#[component]
async fn tool_row(tool: &Tool, on: bool, action: &str) -> Result<impl View> {
    let (effect, effect_class) = match tool.effect {
        Effect::ReadOnly => ("Read-only", "border-border text-muted-foreground"),
        Effect::Writes => ("Writes", "border-border text-foreground"),
        Effect::Destructive => ("Destructive", "border-destructive/40 text-destructive"),
    };
    let about = tool.description.as_deref().or(tool.title.as_deref());

    Ok(view! {
        <li class="flex flex-wrap items-center gap-3 px-4 py-3">
            <div class="flex min-w-56 flex-1 flex-col gap-1">
                <div class="flex flex-wrap items-center gap-2">
                    <span class="font-mono text-sm font-medium break-all">(&tool.name)</span>
                    <span class=(format!("rounded-full border px-2 py-0.5 text-xs {effect_class}"))>(effect)</span>
                    if on {
                        <span class="inline-flex items-center gap-1 text-xs font-medium text-primary">
                            icon(data: iconify_icon!("lucide:circle-check"), size: 12)
                            "On"
                        </span>
                    }
                </div>
                if let Some(about) = about {
                    <p class="line-clamp-2 text-sm text-muted-foreground">(about)</p>
                }
            </div>
            <form method="post" action=(action)>
                <input type="hidden" name="name" value=(&tool.name)>
                <input type="hidden" name="on" value=(if on { "false" } else { "true" })>
                button(
                    variant: if on { ButtonVariant::Ghost } else { ButtonVariant::Outline },
                    size: ButtonSize::Sm,
                    attrs: attributes! { type="submit" },
                    if on { "Turn off" } else { "Turn on" }
                )
            </form>
        </li>
    })
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

/// The form that turns one tool on or off.
#[derive(Deserialize)]
struct ToolForm {
    name: String,
    on: bool,
}

/// Turns one of the app's tools on or off.
#[route(POST "./tools")]
async fn toggle_tool(cx: &Cx, Form(form): Form<ToolForm>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    if !McpTool::set(cx, service.id, &form.name, form.on).await? {
        return Err(bad_request("That is not a tool's name").into());
    }
    Ok(see_other(ServiceTab::Mcp.url(cx, &project, &service)))
}
