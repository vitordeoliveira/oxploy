//! The tools of an organization's apps, which the organization's tokens reach. Each app's
//! own MCP server lists them, the user turned them on in the app's MCP tab, and a token
//! reaches all of them or only the ones picked for it. They are named
//! `<app_name>__<tool>`, and a call goes to the app's server with the tool's own name.
//! See [`app_mcp`].

use serde_json::{Value, json};
use topcoat::context::Cx;

use super::{Failure, INVALID_PARAMS, tool_result};
use crate::db::db;
use crate::features::app_mcp::{self, ConnectError, McpTool, Serving};
use crate::features::deploy::{AppStatus, Deployer};
use crate::features::mcp::Reach;
use crate::features::project::Project;
use crate::features::service::Service;

/// What joins an app's name to the name of one of its tools. App names never hold `_`,
/// so the first one splits them back.
const SEPARATOR: &str = "__";

/// The tools of the organization's apps that the token reaches, as each app's server
/// lists them now, for `tools/list`. An app that is not running, or whose server cannot
/// be reached, lists none until it can.
pub async fn definitions(cx: &Cx, organization_id: u64, reach: &Reach) -> topcoat::Result<Vec<Value>> {
    let mut reached = Vec::new();
    for mut app in app_mcp::serving(cx, organization_id).await? {
        app.enabled = reach.reached(app.service.id, app.enabled);
        if !app.enabled.is_empty() {
            reached.push(app);
        }
    }

    let mut tools = Vec::new();
    for (app, listing) in app_mcp::list_each(cx, reached).await {
        let listing = match listing {
            Some(Ok(listing)) => listing,
            Some(Err(problem)) => {
                eprintln!("oxploy: the MCP tools of {} could not be listed: {problem}", app.service.app_name);
                continue;
            }
            None => continue,
        };
        tools.extend(
            listing
                .tools
                .iter()
                .filter(|tool| app.enabled.contains(&tool.name))
                .map(|tool| passed_on(&tool.definition, &app)),
        );
    }
    Ok(tools)
}

/// Calls the tool `params` names on its app's server, and returns its result as the app
/// gave it. A tool the token does not reach, or of an app of another organization, is
/// unknown, as one that does not exist is. An app that is down, or cannot be reached,
/// makes a failed result the agent can read.
pub async fn call(
    cx: &Cx,
    organization_id: u64,
    reach: &Reach,
    params: &Value,
) -> topcoat::Result<Result<Value, Failure>> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let unknown = || Failure::new(INVALID_PARAMS, format!("Unknown tool: {name}."));
    let Some((app_name, tool)) = split(name) else {
        return Ok(Err(unknown()));
    };
    let Some(service) = reached_app(cx, organization_id, reach, app_name, tool).await? else {
        return Ok(Err(unknown()));
    };
    let (Some(path), Some(token)) = (service.mcp_path.as_deref(), service.mcp_token_in_use()) else {
        return Ok(Err(unknown()));
    };
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

/// The service of the app named `app_name`, if the token reaches its tool `tool`: the
/// app is of the organization, serves MCP, and has the tool turned on, and `reach` keeps
/// it.
async fn reached_app(
    cx: &Cx,
    organization_id: u64,
    reach: &Reach,
    app_name: &str,
    tool: &str,
) -> topcoat::Result<Option<Service>> {
    let Some(service) = Service::filter_by_app_name(app_name).first().exec(&mut db(cx)).await? else {
        return Ok(None);
    };
    if service.mcp_token_in_use().is_none()
        || Project::find_in(cx, organization_id, service.project_id).await?.is_none()
    {
        return Ok(None);
    }
    let reached = reach.reached(service.id, McpTool::enabled_of(cx, service.id).await?);
    Ok(reached.iter().any(|reached| reached == tool).then_some(service))
}

/// A tool as the app listed it, renamed with the app's name in front and described with
/// where it comes from, so an agent tells apart the tools of different apps.
fn passed_on(definition: &Value, app: &Serving) -> Value {
    let mut tool = definition.clone();
    let name = definition.get("name").and_then(Value::as_str).unwrap_or_default();
    let from = format!(
        "From the app {} ({} / {}).",
        app.service.app_name, app.project, app.service.name
    );
    tool["name"] = json!(tool_name(&app.service.app_name, name));
    tool["description"] = json!(match definition.get("description").and_then(Value::as_str) {
        Some(description) => format!("{from} {description}"),
        None => from,
    });
    tool
}

/// The name agents call the app's tool `tool` by, such as `porta-global__list_requests`.
pub(super) fn tool_name(app_name: &str, tool: &str) -> String {
    format!("{app_name}{SEPARATOR}{tool}")
}

/// The app's name and the tool's own name in a name as [`tool_name`] makes it.
pub(super) fn split(name: &str) -> Option<(&str, &str)> {
    name.split_once(SEPARATOR)
        .filter(|(app, tool)| !app.is_empty() && !tool.is_empty())
}

#[cfg(test)]
pub(super) mod tests {
    use topcoat::context::CxTestBuilder;

    use super::super::{Scope, answer};
    use super::*;
    use crate::db::TestDb;
    use crate::features::project::Details;

    /// An app of the organization that serves MCP, with `list_requests` and `purge`
    /// turned on.
    pub(in crate::app::mcp) async fn app_with_tools(cx: &Cx, organization_id: u64, project: &str) -> Service {
        let project = Project::add(cx, organization_id, &Details { name: project, description: "" })
            .await
            .unwrap()
            .unwrap();
        let mut service = Service::add(cx, &project, &Details { name: "Site", description: "" })
            .await
            .unwrap()
            .unwrap();
        service
            .update()
            .mcp_path(Some("/mcp".to_owned()))
            .mcp_token(Some("secret".to_owned()))
            .exec(&mut db(cx))
            .await
            .unwrap();
        for tool in ["list_requests", "purge"] {
            McpTool::set(cx, service.id, tool, true).await.unwrap();
        }
        service
    }

    /// Whether a token of the organization 7 that reaches `reach` may call `tool` of `app`.
    async fn may_call(cx: &Cx, reach: &Reach, app: &Service, tool: &str) -> bool {
        reached_app(cx, 7, reach, &app.app_name, tool).await.unwrap().is_some()
    }

    #[tokio::test]
    async fn a_call_goes_through_only_to_a_tool_the_token_reaches() {
        let test = TestDb::new().await;
        let cx = CxTestBuilder::new().app_context(test.db.clone()).build();
        let app = app_with_tools(&cx, 7, "Porta Global").await;
        let other = app_with_tools(&cx, 8, "Shop").await;
        let picked = Reach::Picked(vec![
            (app.id, "list_requests".to_owned()),
            (app.id, "export".to_owned()),
            (other.id, "purge".to_owned()),
        ]);

        assert!(may_call(&cx, &Reach::All, &app, "list_requests").await);
        assert!(may_call(&cx, &Reach::All, &app, "purge").await);
        assert!(may_call(&cx, &picked, &app, "list_requests").await);
        // Turned on, but not picked.
        assert!(!may_call(&cx, &picked, &app, "purge").await);
        // Picked, but not turned on in the app's MCP tab.
        assert!(!may_call(&cx, &Reach::All, &app, "export").await);
        assert!(!may_call(&cx, &picked, &app, "export").await);
        // Another organization's app, even with the tool picked.
        assert!(!may_call(&cx, &Reach::All, &other, "purge").await);
        assert!(!may_call(&cx, &picked, &other, "purge").await);
    }

    #[tokio::test]
    async fn a_tool_the_token_does_not_reach_is_unknown_as_a_missing_one_is() {
        let test = TestDb::new().await;
        let cx = CxTestBuilder::new().app_context(test.db.clone()).build();
        let app = app_with_tools(&cx, 7, "Porta Global").await;
        let scope = Scope::Organization {
            id: 7,
            name: "Porta Global".to_owned(),
            reach: Reach::Picked(vec![(app.id, "list_requests".to_owned())]),
            token: "Telegram bot".to_owned(),
        };
        let call = |tool: &str| {
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": format!("{}__{tool}", app.app_name), "arguments": {} },
            })
        };

        let not_picked = answer(&cx, &scope, &call("purge")).await.unwrap();
        assert_eq!(not_picked["error"]["code"], INVALID_PARAMS);
        assert_eq!(not_picked["error"]["message"], format!("Unknown tool: {}__purge.", app.app_name));

        let missing = answer(&cx, &scope, &call("drop_tables")).await.unwrap();
        assert_eq!(missing["error"]["code"], INVALID_PARAMS);
        assert_eq!(missing["error"]["message"], format!("Unknown tool: {}__drop_tables.", app.app_name));
    }

    #[test]
    fn splits_a_name_back_into_the_app_and_its_tool() {
        assert_eq!(split("porta-global__list_requests"), Some(("porta-global", "list_requests")));
        assert_eq!(split("shop-api__orders__by_day"), Some(("shop-api", "orders__by_day")));
        assert_eq!(split("server_status"), None);
        assert_eq!(split("__list"), None);
        assert_eq!(split("shop__"), None);
    }
}
