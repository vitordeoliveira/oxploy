//! The people who can sign in to the panel, stored in the database.

use argon2::{Argon2, password_hash::PasswordHasher};
use tokio::sync::Mutex;
use topcoat::{Result, context::Cx};

use crate::db::db;

/// A person who can sign in to the panel.
#[derive(Debug, toasty::Model)]
pub struct User {
    #[key]
    #[auto]
    pub id: u64,

    pub first_name: String,

    pub last_name: String,

    /// Stored trimmed and lowercased, so it matches however the user types it.
    #[unique]
    pub email: String,

    /// The Argon2 hash of the password, as a PHC string.
    pub password_hash: String,

    pub role: Role,
}

/// What a [`User`] is allowed to do.
#[derive(Debug, PartialEq, Eq, toasty::Embed)]
pub enum Role {
    /// Owns the server. The first user created during setup.
    SuperAdmin,
}

/// The fewest characters a password may have.
pub const MIN_PASSWORD_LEN: usize = 8;

/// The details of a user to create.
pub struct NewUser<'a> {
    pub first_name: &'a str,
    pub last_name: &'a str,
    pub email: &'a str,
    pub password: &'a str,
}

/// Serializes setups, so two people finishing it at once cannot both become the super
/// admin.
static SETUP: Mutex<()> = Mutex::const_new(());

impl User {
    /// Returns whether the server has its super admin, which means setup is done.
    pub async fn super_admin_exists(cx: &Cx) -> Result<bool> {
        let super_admin = User::filter(User::fields().role().eq(Role::SuperAdmin))
            .first()
            .exec(&mut db(cx))
            .await?;
        Ok(super_admin.is_some())
    }

    /// Creates the super admin. Returns `false`, creating nothing, if the server
    /// already has one.
    pub async fn create_super_admin(cx: &Cx, new: NewUser<'_>) -> Result<bool> {
        let _setup = SETUP.lock().await;
        if Self::super_admin_exists(cx).await? {
            return Ok(false);
        }

        let password_hash = hash_password(new.password.to_owned()).await?;
        toasty::create!(User {
            first_name: new.first_name.trim(),
            last_name: new.last_name.trim(),
            email: new.email.trim().to_lowercase(),
            password_hash,
            role: Role::SuperAdmin,
        })
        .exec(&mut db(cx))
        .await?;
        Ok(true)
    }
}

/// Hashes `password` with a random salt. Argon2 is slow on purpose, so it runs on a
/// blocking thread instead of an async worker.
async fn hash_password(password: String) -> Result<String> {
    let hash = tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|hash| hash.to_string())
    })
    .await??;
    Ok(hash)
}
