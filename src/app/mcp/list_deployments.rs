//! The `list_deployments` tool: an app's latest deployments, newest first.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::{moment, service_named};
use crate::features::deployment::{self, Deployment, DeploymentStatus};

/// The name agents call the tool by.
pub const NAME: &str = "list_deployments";

/// How many deployments the tool lists when the agent does not say.
const DEFAULT_LIMIT: usize = 10;

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "Lists an app's latest deployments, newest first: each time \
    oxploy fetched its code and built it, or downloaded a GitHub release, and started \
    it. Each has its id, which deployment_log takes; its status: queued, building, \
    starting, succeeded or failed; whether it is the release that runs now; the commit \
    and the first line of its message, or the release's tag and title; when it was \
    asked for and when it finished, and how long it took.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "List an app's deployments",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "app_name": {
                    "type": "string",
                    "description": "The app, by its app_name, as list_services returns it.",
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": deployment::LISTED,
                    "default": DEFAULT_LIMIT,
                    "description": "How many deployments to list.",
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
    limit: Option<usize>,
}

/// Lists the app's deployments. The inner error says what is wrong with the arguments;
/// the outer one is a failure of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    let arguments: Arguments = match super::arguments(arguments) {
        Ok(arguments) => arguments,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let limit = arguments.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=deployment::LISTED).contains(&limit) {
        return Ok(Err(format!("limit is from 1 to {}.", deployment::LISTED)));
    }
    let service = match service_named(cx, &arguments.app_name).await? {
        Ok(service) => service,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let deployments: Vec<_> = Deployment::latest_of(cx, service.id)
        .await?
        .into_iter()
        .take(limit)
        .map(|deployment| {
            let took = deployment
                .finished_at
                .map(|finished| super::duration((finished - deployment.created_at).max(0).unsigned_abs()));
            json!({
                "id": deployment.id,
                "status": status_code(deployment.status),
                "live": service.release_id == Some(deployment.id),
                "commit": deployment.commit,
                "tag": deployment.tag,
                "message": deployment.commit_message,
                "created_at": moment(deployment.created_at),
                "finished_at": deployment.finished_at.and_then(moment),
                "took": took,
            })
        })
        .collect();
    Ok(Ok(json!({
        "app_name": service.app_name,
        "count": deployments.len(),
        "deployments": deployments,
    })))
}

/// A deployment's status, as the tools name it.
pub(super) fn status_code(status: DeploymentStatus) -> &'static str {
    match status {
        DeploymentStatus::Queued => "queued",
        DeploymentStatus::Building => "building",
        DeploymentStatus::Starting => "starting",
        DeploymentStatus::Succeeded => "succeeded",
        DeploymentStatus::Failed => "failed",
    }
}
