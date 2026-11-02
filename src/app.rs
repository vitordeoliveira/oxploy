mod dashboard;
mod login;
mod logout;
mod onboarding;
mod register;

use topcoat::{
    Result,
    asset::{AssetBundle, RouterBuilderAssetExt, asset},
    context::Cx,
    cookie::RouterBuilderCookieExt,
    font::{Font, fontsource::fontsource_font},
    icon::{IconData, iconify::iconify_icon},
    router::{
        RouterBuilder, RouterBuilderDiscoverExt, Slot, error::redirect, href, layout,
        module_router, page,
    },
    runtime::RouterBuilderRuntimeExt,
    session::{RouterBuilderSessionExt, SessionConfig},
    tailwind,
    view::{View, view},
};

use crate::features::{session::UserSession, user::User};

/// The oxploy logo, a cog like the one in the Rust logo.
const LOGO: IconData = iconify_icon!("lucide:cog");

/// The Inter font family, loaded from Fontsource.
const INTER: Font = fontsource_font!(INTER, weight: [400, 500, 600], style: Normal);

/// Builds the application's router.
pub fn router() -> RouterBuilder {
    // The pages and layouts of this module and its submodules, with paths derived
    // from the module tree.
    module_router!()
        // The runtime's procedures and shards, which have no place in the module tree.
        .discover()
        .assets(AssetBundle::load().unwrap())
        // Sign-in sessions, carried in a cookie.
        .cookies()
        .sessions(SessionConfig::default())
        // Lets the browser runtime rerun pages; register it after other layers.
        .runtime()
}

#[layout]
async fn root(slot: Slot<'_>) -> Result<impl View> {
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>"oxploy"</title>
                <link rel="icon" type="image/svg+xml" href=(asset!("assets/favicon.svg"))>
                <link rel="stylesheet" href=(tailwind::stylesheet!())>
                topcoat::font::link(font: INTER)
                topcoat::runtime::script()
                topcoat::dev::script()
            </head>
            <body>(slot)</body>
        </html>
    })
}

/// Sends visitors to setup until the server has its super admin, then to the dashboard
/// or, when they are not signed in, to sign in.
#[page]
async fn home(cx: &Cx) -> Result<()> {
    let target = if UserSession::current(cx).await?.is_some() {
        href!(dashboard::projects::page).resolve(cx)
    } else if User::super_admin_exists(cx).await? {
        href!(login::page).resolve(cx)
    } else {
        href!(register::page).resolve(cx)
    };
    Err(redirect(target).into())
}
