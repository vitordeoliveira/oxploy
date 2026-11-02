use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{error::redirect, href, page},
    runtime::Event,
    view::{View, attributes, element_id, view},
};

use super::onboarding::onboarding;
use crate::components::button::{ButtonSize, button};
use crate::components::field::{field, field_label};
use crate::components::input::input;
use crate::components::password_input::password_input;
use crate::features::user::User;

#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    // Nobody can sign in before setup creates the first user.
    if !User::super_admin_exists(cx).await? {
        return Err(redirect(href!(super::register::page).resolve(cx)).into());
    }

    let email = element_id(cx);
    let password = element_id(cx);

    Ok(view! {
        onboarding(
            <div class="flex flex-col gap-2">
                <h1 class="text-2xl font-semibold tracking-tight">"Welcome back"</h1>
                <p class="text-sm text-muted-foreground">
                    "Sign in to manage your apps and deployments."
                </p>
            </div>
            // Sign-in is not wired yet. The POST keeps the credentials out of the URL if
            // the form is submitted before the runtime loads.
            <form
                class="flex flex-col gap-4"
                method="post"
                @submit=$(|e: Event| e.prevent_default())
            >
                field(
                    field_label(attrs: attributes! { for=(&email) }, "Email")
                    input(
                        attrs: attributes! {
                            id=(&email)
                            type="email"
                            name="email"
                            placeholder="you@example.com"
                            autocomplete="email"
                            required=""
                        }
                    )
                )
                field(
                    field_label(attrs: attributes! { for=(&password) }, "Password")
                    password_input(
                        attrs: attributes! {
                            id=(&password)
                            name="password"
                            placeholder="••••••••"
                            autocomplete="current-password"
                            required=""
                        }
                    )
                )
                button(
                    size: ButtonSize::Lg,
                    attrs: attributes! { type="submit" class="mt-2 w-full" },
                    "Sign in"
                    icon(data: iconify_icon!("lucide:arrow-right"), size: 16)
                )
            </form>
        )
    })
}
