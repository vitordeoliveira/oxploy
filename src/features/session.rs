//! Signed-in browsers, stored in the database.
//!
//! Topcoat issues each browser a random token in a cookie. Only the token's hash is
//! stored here, so a leaked database cannot be used to sign in.

use std::time::{SystemTime, UNIX_EPOCH};

use topcoat::{Result, context::Cx, session};

use super::organization::Organization;
use super::user::User;
use crate::db::db;

/// A browser signed in as a user.
#[derive(Debug, toasty::Model)]
pub struct UserSession {
    #[key]
    #[auto]
    pub id: u64,

    /// The SHA-256 hash of the session token.
    #[unique]
    pub token_hash: Vec<u8>,

    #[index]
    pub user_id: u64,

    /// When the session stops being valid, in seconds since the Unix epoch.
    pub expires_at: i64,

    /// The organization this browser is working in. It may no longer exist; see
    /// [`SignedIn::active_organization`].
    pub active_organization_id: Option<u64>,
}

/// A user's organizations, oldest first, with the positions of two of them.
pub struct Organizations {
    /// Never empty: a user always has at least one organization.
    pub all: Vec<Organization>,
    /// The one this browser is working in.
    pub active: usize,
    /// The one the user opens after signing in.
    pub default: usize,
}

/// The user signed in on the current request, with their session.
pub struct SignedIn {
    pub user: User,
    pub session: UserSession,
}

impl UserSession {
    /// Signs the browser in as `user`, working in `organization`. A fresh token
    /// replaces any the browser already had.
    pub async fn sign_in(cx: &Cx, user: &User, organization: &Organization) -> Result<()> {
        let session = session::start(cx).await?;
        toasty::create!(UserSession {
            token_hash: session.token_hash.to_vec(),
            user_id: user.id,
            expires_at: unix_seconds(session.expires_at),
            active_organization_id: Some(organization.id),
        })
        .exec(&mut db(cx))
        .await?;
        Ok(())
    }

    /// Signs the browser out and forgets its session.
    pub async fn sign_out(cx: &Cx) -> Result<()> {
        if let Some(hash) = session::stop(cx).await? {
            UserSession::filter_by_token_hash(hash.to_vec())
                .delete()
                .exec(&mut db(cx))
                .await?;
        }
        Ok(())
    }

    /// Returns who is signed in on this request, or `None` if nobody is or the session
    /// expired.
    pub async fn current(cx: &Cx) -> Result<Option<SignedIn>> {
        let Some(hash) = session::token_hash(cx).await? else {
            return Ok(None);
        };
        let mut db = db(cx);
        let Some(session) = UserSession::filter_by_token_hash(hash.to_vec())
            .first()
            .exec(&mut db)
            .await?
        else {
            return Ok(None);
        };
        if session.expires_at <= unix_seconds(SystemTime::now()) {
            UserSession::filter_by_id(session.id)
                .delete()
                .exec(&mut db)
                .await?;
            return Ok(None);
        }
        let Some(user) = User::filter_by_id(session.user_id)
            .first()
            .exec(&mut db)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(SignedIn { user, session }))
    }
}

impl SignedIn {
    /// Returns the organizations the user can switch between, marking the one this
    /// browser is working in. When that one is gone, the user's default takes its place.
    pub async fn organizations(&mut self, cx: &Cx) -> Result<Organizations> {
        let default = Organization::default_for(cx, &mut self.user).await?;
        let all = Organization::owned_by(cx, &self.user).await?;
        let position = |id| all.iter().position(|organization| organization.id == id);
        let default = position(default.id).unwrap_or(0);
        let active = self
            .session
            .active_organization_id
            .and_then(position)
            .unwrap_or(default);
        Ok(Organizations {
            all,
            active,
            default,
        })
    }

    /// Switches this browser to `organization`.
    pub async fn activate(&mut self, cx: &Cx, organization: &Organization) -> Result<()> {
        self.session
            .update()
            .active_organization_id(Some(organization.id))
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }
}

fn unix_seconds(time: SystemTime) -> i64 {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    i64::try_from(seconds).unwrap_or(i64::MAX)
}
