//! The `list_mcp_tokens` tool: the MCP tokens of the server and of each organization,
//! and what each one reaches.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::moment;
use super::tokens::{described, turned_on};
use crate::db::db;
use crate::features::app_mcp;
use crate::features::mcp::McpToken;
use crate::features::organization::Organization;

/// The name agents call the tool by.
pub const NAME: &str = "list_mcp_tokens";

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "The MCP tokens clients reach oxploy with. Server tokens \
    read the server, and the ones that manage also install oxploy's updates and manage \
    the organizations' tokens. Each organization comes with its id, the tools turned on \
    in its apps, named as its tokens' clients call them, <app_name>__<tool>, and its \
    tokens, each with its id, whether it reaches all the tools turned on or only the \
    ones picked for it, the tools it reaches now, and, when it is available to the \
    organization's apps, the variable they take it as. Never the tokens' secrets.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "List the MCP tokens",
        "description": DESCRIPTION,
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "openWorldHint": false },
    })
}

/// The tool takes no arguments, and refuses any.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {}

/// Lists the tokens. The inner error says what is wrong with the arguments; the outer
/// one is a failure of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    if let Err(refusal) = super::arguments::<Arguments>(arguments) {
        return Ok(Err(refusal));
    }
    let tokens = McpToken::all()
        .order_by(McpToken::fields().id().asc())
        .exec(&mut db(cx))
        .await?;
    let server: Vec<_> = tokens
        .iter()
        .filter(|token| token.organization_id.is_none())
        .map(|token| {
            json!({
                "id": token.id,
                "name": token.name,
                "manages": token.manages(),
                "created_at": moment(token.created_at),
                "last_used_at": token.last_used_at.and_then(moment),
            })
        })
        .collect();

    let mut organizations = Vec::new();
    let all = Organization::all()
        .order_by(Organization::fields().id().asc())
        .exec(&mut db(cx))
        .await?;
    for organization in all {
        let apps = app_mcp::serving(cx, organization.id).await?;
        let mut theirs = Vec::new();
        for token in tokens.iter().filter(|token| token.organization_id == Some(organization.id)) {
            theirs.push(described(token, &token.reach(cx).await?, &apps));
        }
        organizations.push(json!({
            "id": organization.id,
            "name": organization.name,
            "tools_turned_on": turned_on(&apps),
            "tokens": theirs,
        }));
    }
    Ok(Ok(json!({ "server_tokens": server, "organizations": organizations })))
}
