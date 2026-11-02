mod todos;

use topcoat::{
    Result,
    asset::{AssetBundle, RouterBuilderAssetExt},
    font::{Font, fontsource::fontsource_font},
    icon::{icon, iconify::iconify_icon},
    router::{RouterBuilder, RouterBuilderDiscoverExt, Slot, href, layout, module_router, page},
    runtime::RouterBuilderRuntimeExt,
    tailwind,
    view::{View, attributes, view},
};

use crate::components::{
    button::{ButtonSize, ButtonVariant, button_variants},
    card::{card, card_content, card_description, card_header, card_title},
};

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
                <link rel="stylesheet" href=(tailwind::stylesheet!())>
                topcoat::font::link(font: INTER)
                topcoat::runtime::script()
                topcoat::dev::script()
            </head>
            <body>(slot)</body>
        </html>
    })
}

#[page]
async fn home() -> Result<impl View> {
    Ok(view! {
        <main class="mx-auto max-w-2xl px-4 py-16">
            card(
                card_header(
                    card_title(
                        icon(
                            data: iconify_icon!("lucide:sparkles"),
                            attrs: attributes! { class="inline-block" }
                        )
                        " "
                        "oxploy"
                    )
                    card_description(
                        "Edit "
                        <code>"src/app.rs"</code>
                        " and save to see your changes."
                    )
                )
                card_content(
                    // A link styled like a button, with the button component's classes.
                    <a
                        class=(button_variants(ButtonVariant::Primary, ButtonSize::Md))
                        href=(href!(todos::page))
                    >
                        "Todo example"
                    </a>
                )
            )
        </main>
    })
}
