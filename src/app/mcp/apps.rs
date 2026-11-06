//! The tools of an organization's apps, which the organization's tokens reach. Each app's
//! own MCP server lists them, and the user turned them on in the app's MCP tab. They
//! are named `<app_name>__<tool>`, and a call goes to the app's server with the tool's
//! own name. See [`app_mcp`].

use serde_json::{Value, json};
use topcoat::context::Cx;

use super::{Failure, INVALID_PARAMS, tool_result};
use crate::db::db;
use crate::features::app_mcp::{self, ConnectError, McpTool};
use crate::features::deploy::{AppStatus, Deployer};
use crate::features::project::Project;
use crate::features::service::Service;

/// What joins an app's name to the name of one of its tools. App names never hold `_`,
/// so the first one splits them back.
const SEPARATOR: &str = "__";

/// An app of the organization that serves MCP, with the names of its tools that are on.
struct Serving {
    service: Service,
    project: String,
    enabled: Vec<String>,
}

/// The tools of the organization's apps that are on, as each app's server lists them
/// now, for `tools/list`. An app that is not running, or whose server cannot be reached,
/// lists none until it can.
pub async fn definitions(cx: &Cx, organization_id: u64) -> topcoat::Result<Vec<Value>> {
    let running = Deployer::get(cx);
    let mut asked = Vec::new();
    for serving in serving(cx, organization_id).await? {
        let (Some(port), Some(path), Some(token)) = (
            serving.service.port,
            serving.service.mcp_path.clone(),
            serving.service.mcp_token_in_use().map(str::to_owned),
        ) else {
            continue;
        };
        if running.status(&serving.service) != AppStatus::Running {
            continue;
        }
        // The apps are asked at once, so a slow one only delays the answer by its own time.
        let listing = tokio::spawn(async move { app_mcp::list_tools(port, &path, &token).await });
        asked.push((serving, listing));
    }

    let mut tools = Vec::new();
    for (serving, listing) in asked {
        let listing = match listing.await {
            Ok(Ok(listing)) => listing,
            Ok(Err(problem)) => {
                eprintln!("oxploy: the MCP tools of {} could not be listed: {problem}", serving.service.app_name);
                continue;
            }
            Err(error) => {
                eprintln!("oxploy: the MCP tools of {} could not be listed: {error}", serving.service.app_name);
                continue;
            }
        };
        tools.extend(
            listing
                .tools
                .iter()
                .filter(|tool| serving.enabled.contains(&tool.name))
                .map(|tool| passed_on(&tool.definition, &serving)),
        );
    }
    Ok(tools)
}

/// Calls the tool `params` names on its app's server, and returns its result as the app
/// gave it. A tool that is not on, or of an app of another organization, is unknown. An
/// app that is down, or cannot be reached, makes a failed result the agent can read.
pub async fn call(cx: &Cx, organization_id: u64, params: &Value) -> topcoat::Result<Result<Value, Failure>> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let unknown = || Failure::new(INVALID_PARAMS, format!("Unknown tool: {name}."));
    let Some((app_name, tool)) = split(name) else {
        return Ok(Err(unknown()));
    };
    let Some(service) = Service::filter_by_app_name(app_name).first().exec(&mut db(cx)).await? else {
        return Ok(Err(unknown()));
    };
    let in_organization = Project::find_in(cx, organization_id, service.project_id).await?.is_some();
    let enabled = McpTool::enabled_of(cx, service.id).await?;
    let (Some(path), Some(token)) = (service.mcp_path.as_deref(), service.mcp_token_in_use()) else {
        return Ok(Err(unknown()));
    };
    if !in_organization || !enabled.iter().any(|enabled| enabled == tool) {
        return Ok(Err(unknown()));
    }
    let port = match (Deployer::get(cx).status(&service), service.port) {
        (AppStatus::Running, Some(port)) => port,
        _ => {
            return Ok(Ok(tool_result(
                format!("The app {app_name} is not running, so its tools cannot be called now."),
                true,
            )));
        }
    };
    Ok(match app_mcp::call_tool(port, path, token, tool, arguments).await {
        Ok(result) => Ok(result),
        Err(ConnectError::Rpc { code, message, .. }) => Err(Failure::new(code, message)),
        Err(problem) => Ok(tool_result(format!("The app {app_name} could not run the tool: {problem}."), true)),
    })
}

/// The organization's apps that serve MCP and have tools on.
async fn serving(cx: &Cx, organization_id: u64) -> topcoat::Result<Vec<Serving>> {
    let mut serving = Vec::new();
    for project in Project::in_organization(cx, organization_id).await? {
        for service in Service::in_project(cx, project.id).await? {
            if service.mcp_token_in_use().is_none() {
                continue;
            }
            let enabled = McpTool::enabled_of(cx, service.id).await?;
            if !enabled.is_empty() {
                serving.push(Serving {
                    service,
                    project: project.name.clone(),
                    enabled,
                });
            }
        }
    }
    Ok(serving)
}

/// A tool as the app listed it, renamed with the app's name in front and described with
/// where it comes from, so an agent tells apart the tools of different apps.
fn passed_on(definition: &Value, serving: &Serving) -> Value {
    let mut tool = definition.clone();
    let name = definition.get("name").and_then(Value::as_str).unwrap_or_default();
    let from = format!(
        "From the app {} ({} / {}).",
        serving.service.app_name, serving.project, serving.service.name
    );
    tool["name"] = json!(format!("{}{SEPARATOR}{name}", serving.service.app_name));
    tool["description"] = json!(match definition.get("description").and_then(Value::as_str) {
        Some(description) => format!("{from} {description}"),
        None => from,
    });
    tool
}

/// The app's name and the tool's own name in a name as [`passed_on`] makes it.
fn split(name: &str) -> Option<(&str, &str)> {
    name.split_once(SEPARATOR)
        .filter(|(app, tool)| !app.is_empty() && !tool.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_name_back_into_the_app_and_its_tool() {
        assert_eq!(split("porta-global__list_requests"), Some(("porta-global", "list_requests")));
        assert_eq!(split("shop-api__orders__by_day"), Some(("shop-api", "orders__by_day")));
        assert_eq!(split("server_status"), None);
        assert_eq!(split("__list"), None);
        assert_eq!(split("shop__"), None);
    }
}
