//! Access to the panel's MCP server: the tokens AI agents, such as a bot or Claude Code,
//! send to read the server over the Model Context Protocol.
//!
//! Each token is shown once when it is made. Only its hash is stored, as with the
//! sign-in sessions, so a leaked database cannot be used. A token reaches one of two
//! things: the server, for the super admin, with the machine, oxploy itself and every
//! app it runs; or one organization, with the tools of its apps that were turned on, all
//! of them or only the ones picked for it. See [`Reach`].
//!
//! A server token reads, and one made to manage also installs oxploy's updates and
//! manages the organizations' tokens and the tools turned on in their apps. Server
//! tokens are only made in the panel, so an agent cannot make itself one that manages.

use sha2::{Digest, Sha256};
use toasty::Db;
use topcoat::{
    Result,
    context::Cx,
    router::{header::AUTHORIZATION, request::headers},
};

use super::app_mcp::is_tool_name;
use super::project::now;
use super::service::slug;
use crate::db::db;

/// A token an MCP client sends as `Authorization: Bearer <token>`.
#[derive(Debug, toasty::Model)]
pub struct McpToken {
    #[key]
    #[auto]
    pub id: u64,

    /// What the user calls it, such as the client that uses it.
    pub name: String,

    /// The organization whose apps' tools it reaches, or `None` for a server token.
    #[index]
    pub organization_id: Option<u64>,

    /// The SHA-256 hash of the token.
    #[unique]
    pub token_hash: Vec<u8>,

    /// When it was made, in seconds since the Unix epoch.
    pub created_at: i64,

    /// When a client last sent it, in seconds since the Unix epoch, to within
    /// [`LAST_USED_PRECISION`].
    pub last_used_at: Option<i64>,

    /// Whether it reaches only the tools picked for it, of those turned on in its
    /// organization's apps, rather than all of them. `None`, as for a server token or an
    /// organization's token made before tools could be picked, is no.
    pub picks_tools: Option<bool>,

    /// Whether a server token also manages: installs oxploy's updates, and manages the
    /// organizations' tokens and the tools turned on in their apps. `None`, as for an
    /// organization's token or a server token made before tokens could manage, is no.
    pub manages: Option<bool>,
}

/// A tool of an organization's app that was picked for one of its tokens. It reaches the
/// token only while it is also turned on in the app's MCP tab.
#[derive(Debug, toasty::Model)]
pub struct McpTokenTool {
    #[key]
    #[auto]
    pub id: u64,

    #[index]
    pub token_id: u64,

    /// The service of the app that offers it.
    #[index]
    pub service_id: u64,

    /// The tool's name, as the app's server lists it.
    pub name: String,

    /// When it was picked, in seconds since the Unix epoch.
    pub picked_at: i64,
}

/// Which of the tools turned on in an organization's apps a token reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// All of them, and each one turned on later.
    All,
    /// Only the ones picked for it, each by its app's service and its name.
    Picked(Vec<(u64, String)>),
}

impl Reach {
    /// The names of the service's tools the token reaches, of the names `enabled` in
    /// the app's MCP tab. Listing the tools and calling one both go through it, so a
    /// token never calls a tool it is not shown.
    pub fn reached(&self, service_id: u64, mut enabled: Vec<String>) -> Vec<String> {
        if let Reach::Picked(picked) = self {
            enabled.retain(|name| picked.iter().any(|(id, picked)| *id == service_id && picked == name));
        }
        enabled
    }
}

/// What every token starts with, so one is easy to recognize, as in a leaked file.
const PREFIX: &str = "oxp_";

/// How many random bytes a token carries, after its prefix.
const RANDOM_BYTES: usize = 32;

/// The most characters a token's name may have.
pub const MAX_NAME_LEN: usize = 64;

/// How many seconds `last_used_at` may fall behind before a request updates it, so a
/// busy client does not write to the database on every call.
const LAST_USED_PRECISION: i64 = 60;

impl McpToken {
    /// The tokens of the organization, or the server's with `None`, newest first.
    pub async fn of(cx: &Cx, organization_id: Option<u64>) -> Result<Vec<McpToken>> {
        let mut tokens = McpToken::all()
            .order_by(McpToken::fields().id().desc())
            .exec(&mut db(cx))
            .await?;
        tokens.retain(|token| token.organization_id == organization_id);
        Ok(tokens)
    }

    /// The token with this ID, if it is one of the organization's, or the server's with
    /// `None`.
    pub async fn find_of(cx: &Cx, organization_id: Option<u64>, id: u64) -> Result<Option<McpToken>> {
        let token = McpToken::filter_by_id(id).first().exec(&mut db(cx)).await?;
        Ok(token.filter(|token| token.organization_id == organization_id))
    }

    /// Makes a token named `name` for the organization, or for the server with `None`,
    /// and returns it with the token itself, which is never stored and so cannot be
    /// shown again. An organization's token reaches the tools in `picked`, each by its
    /// app's service and its name, or all of them with `None`; a server token reaches
    /// the server, whatever `picked` says, and also manages with `manages`, which an
    /// organization's token ignores.
    ///
    /// Returns why not instead if the name has no letters or digits, is too long, or
    /// reads like the name of another of the same tokens: as clients know each token by
    /// a name made from it, two such would clash.
    pub async fn issue(
        cx: &Cx,
        organization_id: Option<u64>,
        name: &str,
        picked: Option<&[(u64, String)]>,
        manages: bool,
    ) -> Result<Result<(McpToken, String), IssueError>> {
        let name = name.trim();
        if slug(name).is_empty() || name.chars().count() > MAX_NAME_LEN {
            return Ok(Err(IssueError::BadName));
        }
        let taken = McpToken::of(cx, organization_id)
            .await?
            .iter()
            .any(|token| slug(&token.name) == slug(name));
        if taken {
            return Ok(Err(IssueError::Taken));
        }
        let picked = organization_id.and(picked);
        let secret = new_secret();
        let token = toasty::create!(McpToken {
            name: name.to_owned(),
            organization_id,
            token_hash: hash(&secret),
            created_at: now(),
            last_used_at: None,
            picks_tools: organization_id.map(|_| picked.is_some()),
            manages: organization_id.is_none().then_some(manages),
        })
        .exec(&mut db(cx))
        .await?;
        token.set_picked(cx, picked.unwrap_or_default()).await?;
        Ok(Ok((token, secret)))
    }

    /// Whether it reaches only the tools picked for it.
    pub fn picks_tools(&self) -> bool {
        self.picks_tools == Some(true)
    }

    /// Whether it is a server token that also manages.
    pub fn manages(&self) -> bool {
        self.organization_id.is_none() && self.manages == Some(true)
    }

    /// The token with this ID, if it is an organization's, with that organization's ID.
    pub async fn find_of_organization(cx: &Cx, id: u64) -> Result<Option<(McpToken, u64)>> {
        let token = McpToken::filter_by_id(id).first().exec(&mut db(cx)).await?;
        Ok(token.and_then(|token| token.organization_id.map(|organization_id| (token, organization_id))))
    }

    /// The tools picked for it, each by its app's service and its name, whether or not
    /// it reaches only those.
    pub async fn picked(&self, cx: &Cx) -> Result<Vec<(u64, String)>> {
        let picked = McpTokenTool::filter_by_token_id(self.id)
            .exec(&mut db(cx))
            .await?;
        Ok(picked.into_iter().map(|tool| (tool.service_id, tool.name)).collect())
    }

    /// Which of its organization's tools it reaches.
    pub async fn reach(&self, cx: &Cx) -> Result<Reach> {
        Ok(if self.picks_tools() {
            Reach::Picked(self.picked(cx).await?)
        } else {
            Reach::All
        })
    }

    /// Has it reach only the tools picked for it, or all of them. The tools picked are
    /// kept either way, so going back to picking finds them as they were.
    pub async fn set_picks_tools(&mut self, cx: &Cx, picks: bool) -> Result<()> {
        self.update().picks_tools(Some(picks)).exec(&mut db(cx)).await?;
        Ok(())
    }

    /// Has the tools in `picked` be the ones picked for it, each by its app's service and
    /// its name, in place of those picked before.
    pub async fn set_picked(&self, cx: &Cx, picked: &[(u64, String)]) -> Result<()> {
        McpTokenTool::filter_by_token_id(self.id).delete().exec(&mut db(cx)).await?;
        for (service_id, name) in picked {
            self.pick(cx, *service_id, name, true).await?;
        }
        Ok(())
    }

    /// Picks the service's tool named `name` for it, or leaves it out. Returns `false`,
    /// changing nothing, if the name cannot be a tool's.
    pub async fn pick(&self, cx: &Cx, service_id: u64, name: &str, on: bool) -> Result<bool> {
        if !is_tool_name(name) {
            return Ok(false);
        }
        let mut db = db(cx);
        let existing = McpTokenTool::filter_by_token_id(self.id)
            .exec(&mut db)
            .await?
            .into_iter()
            .filter(|tool| tool.service_id == service_id && tool.name == name);
        for tool in existing {
            McpTokenTool::filter_by_id(tool.id).delete().exec(&mut db).await?;
        }
        if on {
            toasty::create!(McpTokenTool {
                token_id: self.id,
                service_id,
                name: name.to_owned(),
                picked_at: now(),
            })
            .exec(&mut db)
            .await?;
        }
        Ok(true)
    }

    /// Deletes the token, with the tools picked for it. Clients that send it are turned
    /// away from then on.
    pub async fn revoke(self, cx: &Cx) -> Result<()> {
        McpTokenTool::filter_by_token_id(self.id).delete().exec(&mut db(cx)).await?;
        McpToken::filter_by_id(self.id).delete().exec(&mut db(cx)).await?;
        Ok(())
    }

    /// Deletes the organization's tokens, as when it is deleted.
    pub async fn remove_all_in(cx: &Cx, organization_id: u64) -> Result<()> {
        for token in McpToken::of(cx, Some(organization_id)).await? {
            token.revoke(cx).await?;
        }
        Ok(())
    }
}

impl McpTokenTool {
    /// Forgets the tools of a service picked for any token, as when it is deleted.
    pub async fn remove_all(db: &Db, service_id: u64) -> toasty::Result<()> {
        McpTokenTool::filter_by_service_id(service_id)
            .delete()
            .exec(&mut db.clone())
            .await
    }
}

/// Why a token could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueError {
    /// The name has no letters or digits, or is longer than [`MAX_NAME_LEN`].
    BadName,
    /// Another of the same tokens has a name that reads the same.
    Taken,
}

/// The name an MCP client knows a token's server by, such as
/// `oxploy-server-claude-local` for a server token named Claude Local, or
/// `oxploy-shop-telegram-bot` for the organization Shop's token named Telegram bot, so a
/// client that holds several tokens tells them apart.
pub fn client_name(organization: Option<&str>, token: &str) -> String {
    let owner = match organization.map(slug) {
        None => "server".to_owned(),
        Some(organization) if organization.is_empty() => "organization".to_owned(),
        Some(organization) => organization,
    };
    ["oxploy".to_owned(), owner, slug(token)]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// The token the current request carries, if it is one of the tokens, and notes that it
/// was used.
pub async fn authorized(cx: &Cx) -> Result<Option<McpToken>> {
    let Some(given) = headers(cx)
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer)
    else {
        return Ok(None);
    };
    let Some(mut token) = McpToken::filter_by_token_hash(hash(given))
        .first()
        .exec(&mut db(cx))
        .await?
    else {
        return Ok(None);
    };
    let now = now();
    if token.last_used_at.is_none_or(|at| now - at >= LAST_USED_PRECISION) {
        token.update().last_used_at(Some(now)).exec(&mut db(cx)).await?;
    }
    Ok(Some(token))
}

/// A new token: the prefix, then random bytes in hex.
fn new_secret() -> String {
    let mut bytes = [0; RANDOM_BYTES];
    getrandom::fill(&mut bytes).expect("the system provides random bytes");
    let random: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{PREFIX}{random}")
}

/// The SHA-256 hash a token is stored and looked up by.
fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// The token in an `Authorization` header's value, if it uses the Bearer scheme, whose
/// name is case-insensitive.
fn bearer(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("Bearer").then(|| token.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_tokens_that_differ_and_are_easy_to_recognize() {
        let first = new_secret();
        let second = new_secret();
        assert!(first.starts_with(PREFIX));
        assert_eq!(first.len(), PREFIX.len() + RANDOM_BYTES * 2);
        assert_ne!(first, second);
        assert_ne!(hash(&first), hash(&second));
    }

    #[test]
    fn looks_tokens_up_by_their_exact_hash() {
        assert_eq!(hash("oxp_3f9a1c"), hash("oxp_3f9a1c"));
        assert_ne!(hash("oxp_3f9a1c"), hash("oxp_3f9a1c "));
        assert_ne!(hash("oxp_3f9a1c"), hash("OXP_3F9A1C"));
    }

    #[test]
    fn names_each_token_for_its_client() {
        assert_eq!(client_name(None, "Claude Local"), "oxploy-server-claude-local");
        assert_eq!(
            client_name(Some("My Organization"), "telegram_bot"),
            "oxploy-my-organization-telegram-bot"
        );
        assert_eq!(client_name(Some("Ação!"), "Bot"), "oxploy-a-o-bot");
        assert_eq!(client_name(Some("🚀"), "Bot"), "oxploy-organization-bot");
    }

    #[test]
    fn a_token_reaches_the_tools_turned_on_or_only_those_picked_of_them() {
        let enabled = || vec!["list_requests".to_owned(), "archive".to_owned(), "purge".to_owned()];
        assert_eq!(Reach::All.reached(3, enabled()), enabled());

        let picked = Reach::Picked(vec![
            (3, "list_requests".to_owned()),
            // Picked, but turned off in the app's MCP tab.
            (3, "export".to_owned()),
            // A tool of the same name of another app.
            (4, "purge".to_owned()),
        ]);
        assert_eq!(picked.reached(3, enabled()), ["list_requests"]);
        assert_eq!(picked.reached(4, enabled()), ["purge"]);
        assert!(picked.reached(5, enabled()).is_empty());
        assert!(Reach::Picked(Vec::new()).reached(3, enabled()).is_empty());
    }

    #[test]
    fn reads_the_token_of_a_bearer_header() {
        assert_eq!(bearer("Bearer oxp_3f9a1c"), Some("oxp_3f9a1c"));
        assert_eq!(bearer("bearer oxp_3f9a1c"), Some("oxp_3f9a1c"));
        assert_eq!(bearer("Basic YWRtaW46"), None);
        assert_eq!(bearer("oxp_3f9a1c"), None);
    }
}
