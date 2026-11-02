//! The database connection.

use toasty::{Db, migration::MigrationSet};
use topcoat::context::{Cx, app_context};

/// The database used when `DATABASE_URL` is not set: a local SQLite file, created when
/// it is first opened.
const DEFAULT_DATABASE_URL: &str = "sqlite:data.db";

/// The migrations in `toasty/`, built into the binary.
static MIGRATIONS: MigrationSet = toasty::embed_migrations!();

/// Opens the database with every model in the application and applies any pending
/// migrations, so a fresh install creates its own tables.
///
/// After changing a model, generate a migration with
/// `cargo run -- toasty migration generate`.
pub async fn connect() -> toasty::Result<Db> {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let db = Db::builder()
        .models(toasty::models!(crate::*))
        .connect(&url)
        .await?;
    MIGRATIONS.apply(&db).await?;
    Ok(db)
}

/// Returns the database handle from the router's app context. Cloning it is cheap
/// because it refers to a shared connection pool.
pub fn db(cx: &Cx) -> Db {
    app_context::<Db>(cx).clone()
}
