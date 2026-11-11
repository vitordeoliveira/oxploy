mod app;
// Installed components come with variants and helpers the app may not use yet.
#[allow(dead_code)]
mod components;
mod db;
mod features;

#[tokio::main]
async fn main() {
    // An update runs `oxploy --version` to check the binary it downloaded.
    if std::env::args().nth(1).is_some_and(|arg| arg == "--version") {
        println!("oxploy {}", features::update::VERSION);
        return;
    }

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

    // The time zone the super admin picked, which the panel shows times in.
    features::time_zone::PanelTimeZone::load(&db).await.unwrap();
    let monitor = features::monitoring::Monitor::start().unwrap();
    // Keeps Caddy's routes to the apps in step with their domains.
    let caddy = features::caddy::Caddy::start(db.clone());
    // Takes back the apps a previous run started, and starts the ones that should run.
    let deployer = features::deploy::Deployer::start(db.clone(), caddy.clone())
        .await
        .unwrap();
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
    // Looks for a newer release, which the super admin can install from the panel.
    let updater = features::update::Updater::start();
    // Tells oxploy.com once a day that this server runs, unless it is turned off.
    features::telemetry::Telemetry::get_in(&db).await.unwrap();
    features::telemetry::Telemetry::start(db.clone());
    topcoat::start(
        app::router()
            .app_context(db)
            .app_context(monitor)
            .app_context(deployer)
            .app_context(caddy)
            .app_context(setup_code)
            .app_context(updater)
            .build(),
    )
    .await
    .unwrap();
}
