//! The people who can sign in to the panel, stored in the database.

use std::sync::LazyLock;

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
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

    /// The organization to open after signing in. See
    /// [`Organization::default_for`](super::organization::Organization::default_for).
    pub default_organization_id: Option<u64>,
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

    /// Returns the user with this email and password, or `None` if either is wrong.
    pub async fn authenticate(cx: &Cx, email: &str, password: &str) -> Result<Option<User>> {
        let email = email.trim().to_lowercase();
        let user = User::filter_by_email(&email)
            .first()
            .exec(&mut db(cx))
            .await?;
        let hash = user.as_ref().map(|user| user.password_hash.clone());
        let matches = verify_password(hash, password.to_owned()).await?;
        Ok(user.filter(|_| matches))
    }

    /// Makes `organization_id` the organization this user opens after signing in.
    pub async fn set_default_organization(&mut self, cx: &Cx, organization_id: u64) -> Result<()> {
        self.update()
            .default_organization_id(Some(organization_id))
            .exec(&mut db(cx))
            .await?;
        Ok(())
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

/// Checks `password` against `hash`. Without a hash, it checks against a throwaway one
/// and fails, so an unknown email takes as long to reject as a wrong password.
async fn verify_password(hash: Option<String>, password: String) -> Result<bool> {
    static UNKNOWN_USER: LazyLock<String> = LazyLock::new(|| {
        Argon2::default()
            .hash_password(b"no user has this password")
            .expect("hashing a constant password succeeds")
            .to_string()
    });

    let matches = tokio::task::spawn_blocking(move || {
        let known = hash.is_some();
        let hash = hash.unwrap_or_else(|| UNKNOWN_USER.clone());
        let matches = PasswordHash::new(&hash).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        });
        known && matches
    })
    .await?;
    Ok(matches)
}
