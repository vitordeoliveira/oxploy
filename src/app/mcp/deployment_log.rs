//! The `deployment_log` tool: what a deployment printed while fetching, building and
//! starting an app.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::{lines_of, service_named};
use crate::features::app_files::AppFiles;
use crate::features::deployment::Deployment;

/// The name agents call the tool by.
pub const NAME: &str = "deployment_log";

/// How many lines the tool returns when the agent does not say.
const DEFAULT_LINES: usize = 200;

/// The most lines the tool returns.
const MAX_LINES: usize = 1000;

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "The end of a deployment's log: what oxploy and the build \
    printed while fetching the app's code, building it and starting it, ending with \
    why it failed when it did. Without deployment_id, it is the app's latest \
    deployment; list_deployments lists the others.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "Read a deployment's log",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "app_name": {
                    "type": "string",
                    "description": "The app, by its app_name, as list_services returns it.",
                },
                "deployment_id": {
                    "type": "integer",
                    "description": "The deployment, by its id, as list_deployments returns it.",
                },
                "lines": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LINES,
                    "default": DEFAULT_LINES,
                    "description": "How many lines to return, from the end of the log.",
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
    deployment_id: Option<u64>,
    lines: Option<usize>,
}

/// Reads the end of the deployment's log. The inner error says what is wrong with the
/// arguments; the outer one is a failure of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    let arguments: Arguments = match super::arguments(arguments) {
        Ok(arguments) => arguments,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let lines = arguments.lines.unwrap_or(DEFAULT_LINES);
    if !(1..=MAX_LINES).contains(&lines) {
        return Ok(Err(format!("lines is from 1 to {MAX_LINES}.")));
    }
    let service = match service_named(cx, &arguments.app_name).await? {
        Ok(service) => service,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let deployment = match arguments.deployment_id {
        Some(id) => Deployment::find_of(cx, service.id, id).await?,
        None => Deployment::latest_of(cx, service.id).await?.into_iter().next(),
    };
    let Some(deployment) = deployment else {
        return Ok(Err(match arguments.deployment_id {
            Some(id) => format!(
                "{} has no deployment {id}. list_deployments lists its deployments.",
                service.app_name
            ),
            None => format!("{} has not been deployed yet.", service.app_name),
        }));
    };
    let log = lines_of(&AppFiles::of(&service.app_name).deploy_log(deployment.id), lines, None);
    Ok(Ok(json!({
        "app_name": service.app_name,
        "deployment_id": deployment.id,
        "status": super::list_deployments::status_code(deployment.status),
        "lines": log.lines().count(),
        "log": log,
    })))
}
