//! The `app_mcp_tools` tool: the tools an app's own MCP server offers, and which of them
//! are turned on.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::apps::tool_name;
use super::service_named;
use crate::features::app_mcp::{self, Effect, McpTool};
use crate::features::deploy::{AppStatus, Deployer};

/// The name agents call the tool by.
pub const NAME: &str = "app_mcp_tools";

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "The tools an app's own MCP server offers now, asked of the \
    app, each with what it does, what the app declares it does to its data (read_only, \
    writes or destructive), whether it is turned on, and the name its organization's \
    tokens call it by, <app_name>__<tool>. Only the tools turned on reach the \
    organization's tokens. The app must be running to be asked; while it is not, only \
    the names of the tools turned on are known.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "List an app's MCP tools",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "app_name": {
                    "type": "string",
                    "description": "The app, by its app_name, as list_services returns it.",
                },
            },
            "required": ["app_name"],
            "additionalProperties": false,
        },
        "annotations": { "readOnlyHint": true, "openWorldHint": false },
    })
}

/// The tool's arguments, as the agent sends them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    app_name: String,
}

/// Asks the app's MCP server for its tools. The inner error says what is wrong with the
/// arguments or the app; the outer one is a failure of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    let arguments: Arguments = match super::arguments(arguments) {
        Ok(arguments) => arguments,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let service = match service_named(cx, &arguments.app_name).await? {
        Ok(service) => service,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let (Some(path), Some(token)) = (service.mcp_path.as_deref(), service.mcp_token_in_use()) else {
        return Ok(Err(format!(
            "The app {} serves no MCP. Its MCP tab in the panel turns that on.",
            service.app_name
        )));
    };
    let enabled = McpTool::enabled_of(cx, service.id).await?;
    let port = match (Deployer::get(cx).status(&service), service.port) {
        (AppStatus::Running, Some(port)) => port,
        _ => {
            return Ok(Ok(json!({
                "app_name": service.app_name,
                "running": false,
                "tools_turned_on": enabled,
            })));
        }
    };
    let listing = match app_mcp::list_tools(port, path, token).await {
        Ok(listing) => listing,
        Err(problem) => {
            return Ok(Err(format!("The app {} could not be asked: {problem}.", service.app_name)));
        }
    };
    let tools: Vec<_> = listing
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "called_as": tool_name(&service.app_name, &tool.name),
                "title": tool.title,
                "description": tool.description,
                "effect": effect_code(tool.effect),
                "turned_on": enabled.contains(&tool.name),
            })
        })
        .collect();
    Ok(Ok(json!({
        "app_name": service.app_name,
        "running": true,
        "server": listing.server.title.as_ref().unwrap_or(&listing.server.name),
        "tools": tools,
    })))
}

/// What a tool does to its app's data, as the tool names it.
pub(super) fn effect_code(effect: Effect) -> &'static str {
    match effect {
        Effect::ReadOnly => "read_only",
        Effect::Writes => "writes",
        Effect::Destructive => "destructive",
    }
}
