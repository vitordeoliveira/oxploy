//! The `app_logs` tool: what a running app printed.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::{lines_of, service_named};
use crate::features::app_files::AppFiles;

/// The name agents call the tool by.
pub const NAME: &str = "app_logs";

/// How many lines the tool returns when the agent does not say.
const DEFAULT_LINES: usize = 100;

/// The most lines the tool returns.
const MAX_LINES: usize = 1000;

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "The end of an app's log: what it printed while it ran, \
    across restarts and deployments. oxploy adds lines of its own, which start with \
    ==> and the time: when it starts a release, when the app exits or cannot start, and \
    for each request the app answered with a server error. contains keeps only the \
    lines with that text, ignoring case, searched in the last 256 KiB of the log.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "Read an app's log",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "app_name": {
                    "type": "string",
                    "description": "The app, by its app_name, as list_services returns it.",
                },
                "lines": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LINES,
                    "default": DEFAULT_LINES,
                    "description": "How many lines to return, from the end of the log.",
                },
                "contains": {
                    "type": "string",
                    "description": "Only the lines with this text, ignoring case, such as error.",
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
    lines: Option<usize>,
    contains: Option<String>,
}

/// Reads the end of the app's log. The inner error says what is wrong with the
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
    let contains = arguments.contains.as_deref().filter(|text| !text.is_empty());
    let log = lines_of(&AppFiles::of(&service.app_name).app_log(), lines, contains);
    Ok(Ok(json!({
        "app_name": service.app_name,
        "lines": log.lines().count(),
        "log": log,
    })))
}
