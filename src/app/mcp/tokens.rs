//! What the tools that read and manage the organizations' MCP tokens share: how they
//! describe a token, and how they find the tools an agent names.

use serde_json::{Value, json};
use topcoat::context::Cx;

use super::apps::{split, tool_name};
use super::moment;
use crate::features::app_mcp::{self, Serving};
use crate::features::mcp::{McpToken, Reach};

/// The tools turned on in `apps`, by the names agents call them, such as
/// `porta-global__list_requests`.
pub fn turned_on(apps: &[Serving]) -> Vec<String> {
    apps.iter()
        .flat_map(|app| app.enabled.iter().map(|tool| tool_name(&app.service.app_name, tool)))
        .collect()
}

/// An organization's token as the tools describe it: whether it reaches every tool
/// turned on in `apps`, its organization's apps, or only the ones picked for it, and the
/// tools it reaches now. Never its secret, which oxploy does not keep.
pub fn described(token: &McpToken, reach: &Reach, apps: &[Serving]) -> Value {
    let tools: Vec<_> = apps
        .iter()
        .flat_map(|app| {
            reach
                .reached(app.service.id, app.enabled.clone())
                .into_iter()
                .map(|tool| tool_name(&app.service.app_name, &tool))
        })
        .collect();
    json!({
        "id": token.id,
        "name": token.name,
        "reaches": if token.picks_tools() { "picked" } else { "all" },
        "tools": tools,
        "created_at": moment(token.created_at),
        "last_used_at": token.last_used_at.and_then(moment),
    })
}

/// [`described`], for a token of the organization `organization_id`.
pub async fn describe(cx: &Cx, token: &McpToken, organization_id: u64) -> topcoat::Result<Value> {
    let apps = app_mcp::serving(cx, organization_id).await?;
    Ok(described(token, &token.reach(cx).await?, &apps))
}

/// The tools `names` names, such as `porta-global__list_requests`, each by its app's
/// service and its own name, as a token picks them. Each must be turned on in one of
/// the organization's apps; the inner error says which one is not.
pub async fn picks_named(
    cx: &Cx,
    organization_id: u64,
    names: &[String],
) -> topcoat::Result<Result<Vec<(u64, String)>, String>> {
    let apps = app_mcp::serving(cx, organization_id).await?;
    let mut picked = Vec::new();
    for name in names {
        let found = split(name).and_then(|(app_name, tool)| {
            apps.iter()
                .find(|app| app.service.app_name == app_name && app.enabled.iter().any(|on| on == tool))
                .map(|app| (app.service.id, tool.to_owned()))
        });
        match found {
            Some(pick) if picked.contains(&pick) => {}
            Some(pick) => picked.push(pick),
            None => {
                return Ok(Err(format!(
                    "{name} is not a tool turned on in this organization's apps. \
                     list_mcp_tokens lists the ones that are, and set_app_tools turns one on."
                )));
            }
        }
    }
    Ok(Ok(picked))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use topcoat::context::CxTestBuilder;

    use super::super::apps::tests::app_with_tools;
    use super::super::{create_mcp_token, revoke_mcp_token, set_token_tools};
    use crate::db::{TestDb, db};
    use crate::features::mcp::McpToken;
    use crate::features::organization::Organization;

    /// An organization named `name`.
    async fn organization(cx: &topcoat::context::Cx, name: &str) -> Organization {
        toasty::create!(Organization {
            name: name.to_owned(),
            logo_url: None,
            owner_id: 1,
        })
        .exec(&mut db(cx))
        .await
        .unwrap()
    }

    fn arguments(organization_id: u64, name: &str, tools: &[&str]) -> create_mcp_token::Arguments {
        create_mcp_token::Arguments {
            organization_id,
            name: name.to_owned(),
            all: false,
            tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn an_agent_makes_a_token_that_reaches_the_tools_it_picks() {
        let test = TestDb::new().await;
        let cx = CxTestBuilder::new().app_context(test.db.clone()).build();
        let shop = organization(&cx, "Porta Global").await;
        let app = app_with_tools(&cx, shop.id, "Porta Global").await;
        let endpoint = "https://panel.test/mcp";
        let tool = |name: &str| format!("{}__{name}", app.app_name);

        let made = create_mcp_token::issue(&cx, arguments(shop.id, "Telegram bot", &[&tool("list_requests")]), endpoint)
            .await
            .unwrap()
            .unwrap();
        let secret = made["secret"].as_str().unwrap();
        assert!(secret.starts_with("oxp_"));
        assert_eq!(made["token"]["reaches"], "picked");
        assert_eq!(made["token"]["tools"], json!([tool("list_requests")]));
        let command = made["claude_code_command"].as_str().unwrap();
        assert!(command.contains("oxploy-porta-global-telegram-bot https://panel.test/mcp"), "{command}");
        let stored = McpToken::filter_by_id(made["token"]["id"].as_u64().unwrap())
            .first()
            .exec(&mut db(&cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.organization_id, Some(shop.id));
        assert!(!stored.manages());

        // A tool that is not turned on, a name already taken, and an organization that
        // does not exist make nothing.
        let refusals = [
            arguments(shop.id, "Other", &[&tool("export")]),
            arguments(shop.id, "telegram-bot", &[]),
            arguments(99, "Other", &[]),
        ];
        for refused in refusals {
            assert!(create_mcp_token::issue(&cx, refused, endpoint).await.unwrap().is_err());
        }
        assert_eq!(McpToken::of(&cx, Some(shop.id)).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_agent_changes_and_revokes_only_the_organizations_tokens() {
        let test = TestDb::new().await;
        let cx = CxTestBuilder::new().app_context(test.db.clone()).build();
        let shop = organization(&cx, "Porta Global").await;
        let app = app_with_tools(&cx, shop.id, "Porta Global").await;
        let (bot, _) = McpToken::issue(&cx, Some(shop.id), "Telegram bot", Some(&[]), false)
            .await
            .unwrap()
            .unwrap();
        let (server, _) = McpToken::issue(&cx, None, "Claude Local", None, true).await.unwrap().unwrap();
        assert!(server.manages());

        let purge = format!("{}__purge", app.app_name);
        let set = json!({ "token_id": bot.id, "all": false, "tools": [purge] });
        let changed = set_token_tools::call(&cx, set).await.unwrap().unwrap();
        assert_eq!(changed["token"]["tools"], json!([purge]));
        let set = json!({ "token_id": bot.id, "all": true });
        let changed = set_token_tools::call(&cx, set).await.unwrap().unwrap();
        assert_eq!(changed["token"]["reaches"], "all");
        assert_eq!(changed["token"]["tools"].as_array().unwrap().len(), 2);

        // A server token is changed and revoked only in the panel.
        let set = json!({ "token_id": server.id, "all": true });
        assert!(set_token_tools::call(&cx, set).await.unwrap().is_err());
        let revoke = json!({ "token_id": server.id });
        assert!(revoke_mcp_token::call(&cx, revoke).await.unwrap().is_err());

        let revoke = json!({ "token_id": bot.id });
        let revoked = revoke_mcp_token::call(&cx, revoke).await.unwrap().unwrap();
        assert_eq!(revoked["revoked"], "Telegram bot");
        assert!(McpToken::of(&cx, Some(shop.id)).await.unwrap().is_empty());
        assert_eq!(McpToken::of(&cx, None).await.unwrap().len(), 1);
    }
}
