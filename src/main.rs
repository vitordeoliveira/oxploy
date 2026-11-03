mod app;
// Installed components come with variants and helpers the app may not use yet.
#[allow(dead_code)]
mod components;
mod db;
mod features;

#[tokio::main]
async fn main() {
    let db = db::connect().await.unwrap();

    // `cargo run -- toasty migration <command>` manages the database schema.
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "toasty") {
        if let Err(error) = toasty_cli::ToastyCli::new(db).parse_from(args).await {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }

    let monitor = features::monitoring::Monitor::start().unwrap();
    // Takes back the apps a previous run started, and starts the ones that should run.
    let deployer = features::deploy::Deployer::start(db.clone()).await.unwrap();
    // Keeps Caddy's routes to the apps in step with their domains.
    let caddy = features::caddy::Caddy::start(db.clone());
    // Until the super admin exists, creating it asks for a code from the log.
    let open_at = caddy
        .ports()
        .panel_urls()
        .into_iter()
        .next()
        .unwrap_or_else(|| format!("http://{}", caddy.ports().panel));
    let setup_code = features::setup_code::SetupCode::start(&db, &open_at)
        .await
        .unwrap();
    topcoat::start(
        app::router()
            .app_context(db)
            .app_context(monitor)
            .app_context(deployer)
            .app_context(caddy)
            .app_context(setup_code)
            .build(),
    )
    .await
    .unwrap();
}
