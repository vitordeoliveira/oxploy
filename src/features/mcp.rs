//! Access to the panel's MCP server: the tokens AI agents, such as a bot or Claude Code,
//! send to read the server over the Model Context Protocol.
//!
//! Each token is made in the panel, which shows it once. Only its hash is stored, as
//! with the sign-in sessions, so a leaked database cannot be used. A token reaches one
//! of two things: the server, for the super admin, with the machine, oxploy itself and
//! every app it runs; or one organization, with the tools of its apps that were turned
//! on.

use sha2::{Digest, Sha256};
use topcoat::{
    Result,
    context::Cx,
    router::{header::AUTHORIZATION, request::headers},
};

use super::project::now;
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
    /// shown again. Returns `None` if the name is empty or too long.
    pub async fn issue(
        cx: &Cx,
        organization_id: Option<u64>,
        name: &str,
    ) -> Result<Option<(McpToken, String)>> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
            return Ok(None);
        }
        let secret = new_secret();
        let token = toasty::create!(McpToken {
            name: name.to_owned(),
            organization_id,
            token_hash: hash(&secret),
            created_at: now(),
            last_used_at: None,
        })
        .exec(&mut db(cx))
        .await?;
        Ok(Some((token, secret)))
    }

    /// Deletes the token. Clients that send it are turned away from then on.
    pub async fn revoke(self, cx: &Cx) -> Result<()> {
        McpToken::filter_by_id(self.id).delete().exec(&mut db(cx)).await?;
        Ok(())
    }

    /// Deletes the organization's tokens, as when it is deleted.
    pub async fn remove_all_in(cx: &Cx, organization_id: u64) -> Result<()> {
        McpToken::filter_by_organization_id(Some(organization_id))
            .delete()
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }
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
    fn reads_the_token_of_a_bearer_header() {
        assert_eq!(bearer("Bearer oxp_3f9a1c"), Some("oxp_3f9a1c"));
        assert_eq!(bearer("bearer oxp_3f9a1c"), Some("oxp_3f9a1c"));
        assert_eq!(bearer("Basic YWRtaW46"), None);
        assert_eq!(bearer("oxp_3f9a1c"), None);
    }
}
