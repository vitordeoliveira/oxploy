use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{redirect, see_other},
        href, page,
    },
    view::{View, attributes, component, element_id, view},
};

use super::onboarding::onboarding;
use crate::components::button::{ButtonSize, button};
use crate::components::field::{field, field_label};
use crate::components::input::input;
use crate::components::password_input::password_input;
use crate::features::{organization::Organization, session::UserSession, user::User};

/// The sign-in form, as the browser submits it.
#[derive(Default, Deserialize)]
#[serde(default)]
struct SignInForm {
    email: String,
    password: String,
}

#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    if UserSession::current(cx).await?.is_some() {
        return Err(redirect(href!(super::dashboard::projects::page).resolve(cx)).into());
    }
    // Nobody can sign in before setup creates the first user.
    if !User::super_admin_exists(cx).await? {
        return Err(redirect(href!(super::register::page).resolve(cx)).into());
    }

    Ok(view! { sign_in(email: String::new(), error: None) })
}

/// Signs the user in and opens their default organization, or shows the form again
/// with an error.
#[page(POST)]
async fn submit(cx: &Cx, Form(form): Form<SignInForm>) -> Result<impl View> {
    if let Some(mut user) = User::authenticate(cx, &form.email, &form.password).await? {
        let organization = Organization::default_for(cx, &mut user).await?;
        UserSession::sign_in(cx, &user, &organization).await?;
        return Err(see_other(href!(super::dashboard::projects::page).resolve(cx)).into());
    }

    // The same message for an unknown email and a wrong password, so the form does
    // not reveal which accounts exist.
    Ok(view! { sign_in(email: form.email, error: Some("Invalid email or password")) })
}

/// The sign-in page: the form with `email` filled in, and `error` above it.
#[component]
async fn sign_in(cx: &Cx, email: String, error: Option<&'static str>) -> Result<impl View> {
    let email_id = element_id(cx);
    let password_id = element_id(cx);

    Ok(view! {
        onboarding(
            <div class="flex flex-col gap-2">
                <h1 class="text-2xl font-semibold tracking-tight">"Welcome back"</h1>
                <p class="text-sm text-muted-foreground">
                    "Sign in to manage your apps and deployments."
                </p>
            </div>
            if let Some(error) = error {
                <div
                    role="alert"
                    class="flex items-center gap-2 rounded-lg bg-destructive/10 p-3 text-sm text-destructive"
                >
                    icon(data: iconify_icon!("lucide:triangle-alert"), size: 16)
                    (error)
                </div>
            }
            <form class="flex flex-col gap-4" method="post">
                field(
                    field_label(attrs: attributes! { for=(&email_id) }, "Email")
                    input(
                        attrs: attributes! {
                            id=(&email_id)
                            type="email"
                            name="email"
                            value=(&email)
                            placeholder="you@example.com"
                            autocomplete="email"
                            required=""
                        }
                    )
                )
                field(
                    field_label(attrs: attributes! { for=(&password_id) }, "Password")
                    password_input(
                        attrs: attributes! {
                            id=(&password_id)
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
