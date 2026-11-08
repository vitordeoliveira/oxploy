//! The `set_token_tools` tool: which of its organization's tools a token reaches, as
//! its Tools dialog in the panel sets it.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::tokens::{describe, picks_named};
use crate::features::mcp::McpToken;

/// The name agents call the tool by.
pub const NAME: &str = "set_token_tools";

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "Sets which tools an organization's token reaches, as its \
    Tools dialog in the panel does: every tool turned on in the organization's apps, \
    including the ones turned on later, or only the ones picked for it. tools, when \
    given, replaces the tools picked, each named as list_mcp_tokens lists them, \
    <app_name>__<tool>, and each turned on in one of the organization's apps; the \
    tools picked are kept while the token reaches all of them. Server tokens are \
    managed in the panel. Clients already connected see a change once they reconnect.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "Set the tools an MCP token reaches",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "token_id": {
                    "type": "integer",
                    "description": "The token, by its id, as list_mcp_tokens returns it.",
                },
                "all": {
                    "type": "boolean",
                    "description": "Whether it reaches every tool turned on, rather than only the ones picked.",
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The tools to pick, replacing the ones picked, such as porta-global__list_requests.",
                },
            },
            "required": ["token_id", "all"],
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
    token_id: u64,
    all: bool,
    tools: Option<Vec<String>>,
}

/// Sets what the token reaches, and returns it as list_mcp_tokens does. The inner error
/// says what is wrong with the arguments, changing nothing; the outer one is a failure
/// of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    let arguments: Arguments = match super::arguments(arguments) {
        Ok(arguments) => arguments,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let Some((mut token, organization_id)) = McpToken::find_of_organization(cx, arguments.token_id).await? else {
        return Ok(Err(no_such_token(arguments.token_id)));
    };
    if let Some(tools) = &arguments.tools {
        let picked = match picks_named(cx, organization_id, tools).await? {
            Ok(picked) => picked,
            Err(refusal) => return Ok(Err(refusal)),
        };
        token.set_picked(cx, &picked).await?;
    }
    token.set_picks_tools(cx, !arguments.all).await?;
    Ok(Ok(json!({ "token": describe(cx, &token, organization_id).await? })))
}

/// Why a token ID names no token the tools manage.
pub(super) fn no_such_token(id: u64) -> String {
    format!(
        "There is no organization's token with the id {id}. list_mcp_tokens lists them; \
         server tokens are managed in the panel."
    )
}
