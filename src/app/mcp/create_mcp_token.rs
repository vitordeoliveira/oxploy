//! The `create_mcp_token` tool: makes a token for one of an organization's clients, as
//! the panel's New token button does.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::tokens::{describe, picks_named};
use crate::app::dashboard::panel_url;
use crate::db::db;
use crate::features::mcp::{IssueError, MAX_NAME_LEN, McpToken, client_name};
use crate::features::organization::Organization;

/// The name agents call the tool by.
pub const NAME: &str = "create_mcp_token";

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "Makes an MCP token for one client of an organization, such \
    as a bot, as the panel's New token button does. It reaches the tools picked for it, \
    each named as list_mcp_tokens lists them, <app_name>__<tool>, or, with all, every \
    tool turned on in the organization's apps. It returns the token's secret this once, \
    as oxploy keeps only its hash, with the command that connects Claude Code to it: \
    hand them to the client they are for, and never repeat the secret in a reply. A \
    token of the server cannot be made here.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "Make an MCP token",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "organization_id": {
                    "type": "integer",
                    "description": "The organization, by its id, as list_mcp_tokens returns it.",
                },
                "name": {
                    "type": "string",
                    "maxLength": MAX_NAME_LEN,
                    "description": "What uses it, such as Telegram bot. No other token of the organization may have a name that reads the same.",
                },
                "all": {
                    "type": "boolean",
                    "default": false,
                    "description": "Whether it reaches every tool turned on, rather than only the ones in tools.",
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The tools it reaches, such as porta-global__list_requests, when all is false.",
                },
            },
            "required": ["organization_id", "name"],
            "additionalProperties": false,
        },
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": false,
            "openWorldHint": false,
        },
    })
}

/// The tool's arguments, as the agent sends them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Arguments {
    pub organization_id: u64,
    pub name: String,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub tools: Vec<String>,
}

/// Makes the token. The inner error says what is wrong with the arguments, making
/// nothing; the outer one is a failure of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    let arguments: Arguments = match super::arguments(arguments) {
        Ok(arguments) => arguments,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let endpoint = format!("{}/mcp", panel_url(cx).unwrap_or_default());
    issue(cx, arguments, &endpoint).await
}

/// Makes the token the arguments describe, for clients that reach oxploy at `endpoint`.
pub(super) async fn issue(cx: &Cx, arguments: Arguments, endpoint: &str) -> topcoat::Result<Result<Value, String>> {
    let Some(organization) = Organization::filter_by_id(arguments.organization_id)
        .first()
        .exec(&mut db(cx))
        .await?
    else {
        return Ok(Err(format!(
            "There is no organization with the id {}. list_mcp_tokens lists them.",
            arguments.organization_id
        )));
    };
    if arguments.all && !arguments.tools.is_empty() {
        return Ok(Err("Give tools only when all is false.".to_owned()));
    }
    let picked = if arguments.all {
        None
    } else {
        match picks_named(cx, organization.id, &arguments.tools).await? {
            Ok(picked) => Some(picked),
            Err(refusal) => return Ok(Err(refusal)),
        }
    };
    let (token, secret) = match McpToken::issue(cx, Some(organization.id), &arguments.name, picked.as_deref(), false).await? {
        Ok(issued) => issued,
        Err(IssueError::BadName) => {
            return Ok(Err(format!(
                "Give it a name with letters or digits, of up to {MAX_NAME_LEN} characters."
            )));
        }
        Err(IssueError::Taken) => {
            return Ok(Err(format!(
                "{} already has a token named like {}. Pick another name, so each client gets a name of its own.",
                organization.name,
                arguments.name.trim()
            )));
        }
    };
    let client = client_name(Some(&organization.name), &token.name);
    Ok(Ok(json!({
        "token": describe(cx, &token, organization.id).await?,
        "organization": organization.name,
        "secret": secret,
        "endpoint": endpoint,
        "claude_code_command": format!(
            "claude mcp add --transport http {client} {endpoint} --header \"Authorization: Bearer {secret}\""
        ),
    })))
}
