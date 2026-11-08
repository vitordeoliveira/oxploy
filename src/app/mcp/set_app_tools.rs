//! The `set_app_tools` tool: turns tools of an app's MCP server on or off, as the
//! switches in its MCP tab do.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::service_named;
use crate::features::app_mcp::{self, McpTool};
use crate::features::deploy::{AppStatus, Deployer};

/// The name agents call the tool by.
pub const NAME: &str = "set_app_tools";

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "Turns tools of an app's MCP server on or off, as the \
    switches in its MCP tab in the panel do. A tool turned on reaches the tokens of the \
    app's organization that reach every tool, and the ones that have it picked; one \
    turned off reaches no token, and comes back to the ones that had it picked when it \
    is turned on again. Tools are named as the app names them, without the app's name \
    in front, as app_mcp_tools lists them. Turning one on needs the app running, to \
    check that it offers it. Clients already connected see a change once they \
    reconnect.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "Turn an app's MCP tools on or off",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "app_name": {
                    "type": "string",
                    "description": "The app, by its app_name, as list_services returns it.",
                },
                "turn_on": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The tools to turn on, such as list_requests.",
                },
                "turn_off": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The tools to turn off.",
                },
            },
            "required": ["app_name"],
            "additionalProperties": false,
        },
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": true,
            "openWorldHint": false,
        },
    })
}

/// The tool's arguments, as the agent sends them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    app_name: String,
    #[serde(default)]
    turn_on: Vec<String>,
    #[serde(default)]
    turn_off: Vec<String>,
}

/// Turns the tools on and off, and returns the ones turned on now. The inner error says
/// what is wrong with the arguments or the app, changing nothing; the outer one is a
/// failure of the database.
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
    let all = arguments.turn_on.iter().chain(&arguments.turn_off);
    if let Some(bad) = all.into_iter().find(|name| !app_mcp::is_tool_name(name)) {
        return Ok(Err(format!("{bad:?} cannot be a tool's name.")));
    }
    if let Some(both) = arguments.turn_on.iter().find(|name| arguments.turn_off.contains(name)) {
        return Ok(Err(format!("{both} is both to turn on and to turn off.")));
    }
    if !arguments.turn_on.is_empty() {
        let port = match (Deployer::get(cx).status(&service), service.port) {
            (AppStatus::Running, Some(port)) => port,
            _ => {
                return Ok(Err(format!(
                    "The app {} is not running, so it cannot be checked that it offers the tools. Start it first.",
                    service.app_name
                )));
            }
        };
        let listing = match app_mcp::list_tools(port, path, token).await {
            Ok(listing) => listing,
            Err(problem) => {
                return Ok(Err(format!("The app {} could not be asked: {problem}.", service.app_name)));
            }
        };
        let offered = |name: &String| listing.tools.iter().any(|tool| &tool.name == name);
        if let Some(missing) = arguments.turn_on.iter().find(|name| !offered(name)) {
            return Ok(Err(format!(
                "The app {} offers no tool named {missing}. app_mcp_tools lists the ones it offers.",
                service.app_name
            )));
        }
    }
    for (names, on) in [(&arguments.turn_on, true), (&arguments.turn_off, false)] {
        for name in names {
            McpTool::set(cx, service.id, name, on).await?;
        }
    }
    Ok(Ok(json!({
        "app_name": service.app_name,
        "tools_turned_on": McpTool::enabled_of(cx, service.id).await?,
    })))
}
