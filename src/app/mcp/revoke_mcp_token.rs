//! The `revoke_mcp_token` tool: revokes one of an organization's tokens, as the panel's
//! trash button does.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::set_token_tools::no_such_token;
use crate::db::db;
use crate::features::mcp::McpToken;
use crate::features::organization::Organization;

/// The name agents call the tool by.
pub const NAME: &str = "revoke_mcp_token";

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "Revokes an organization's MCP token, as the panel's trash \
    button does: its client is turned away from then on, and a revoked token cannot be \
    brought back. Server tokens are revoked in the panel.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "Revoke an MCP token",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "token_id": {
                    "type": "integer",
                    "description": "The token, by its id, as list_mcp_tokens returns it.",
                },
            },
            "required": ["token_id"],
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
}

/// Revokes the token. The inner error says what is wrong with the arguments; the outer
/// one is a failure of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    let arguments: Arguments = match super::arguments(arguments) {
        Ok(arguments) => arguments,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let Some((token, organization_id)) = McpToken::find_of_organization(cx, arguments.token_id).await? else {
        return Ok(Err(no_such_token(arguments.token_id)));
    };
    let organization = Organization::filter_by_id(organization_id)
        .first()
        .exec(&mut db(cx))
        .await?
        .map(|organization| organization.name);
    let name = token.name.clone();
    token.revoke(cx).await?;
    Ok(Ok(json!({ "revoked": name, "organization": organization })))
}
