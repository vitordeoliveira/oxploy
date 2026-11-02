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
    view::{Attributes, View, attributes, component, element_id, view},
};

use super::onboarding::onboarding;
use crate::components::button::{ButtonSize, button};
use crate::components::field::{field, field_error, field_label};
use crate::components::input::input;
use crate::components::password_input::password_input;
use crate::features::user::{MIN_PASSWORD_LEN, NewUser, User};

/// The setup form, as the browser submits it.
#[derive(Default, Deserialize)]
#[serde(default)]
struct SetupForm {
    first_name: String,
    last_name: String,
    email: String,
    password: String,
    confirm_password: String,
}

/// What to fix in each field of a [`SetupForm`], if anything.
#[derive(Default)]
struct SetupErrors {
    first_name: Option<&'static str>,
    last_name: Option<&'static str>,
    email: Option<&'static str>,
    password: Option<&'static str>,
    confirm_password: Option<&'static str>,
}

impl SetupErrors {
    fn is_empty(&self) -> bool {
        self.first_name.is_none()
            && self.last_name.is_none()
            && self.email.is_none()
            && self.password.is_none()
            && self.confirm_password.is_none()
    }
}

/// Shows the form that creates the super admin, until setup is done.
#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    if User::super_admin_exists(cx).await? {
        return Err(redirect(href!(super::login::page).resolve(cx)).into());
    }

    Ok(view! { setup(form: SetupForm::default(), errors: SetupErrors::default()) })
}

/// Creates the super admin and sends them to sign in, or shows the form again with
/// what to fix.
#[page(POST)]
async fn submit(cx: &Cx, Form(mut form): Form<SetupForm>) -> Result<impl View> {
    let login = href!(super::login::page).resolve(cx);
    if User::super_admin_exists(cx).await? {
        return Err(see_other(login).into());
    }

    let errors = validate(&form);
    if errors.is_empty() {
        // Setup is done either way: by this request, or by one that finished first.
        User::create_super_admin(
            cx,
            NewUser {
                first_name: &form.first_name,
                last_name: &form.last_name,
                email: &form.email,
                password: &form.password,
            },
        )
        .await?;
        return Err(see_other(login).into());
    }

    // The passwords are never sent back to the browser.
    form.password.clear();
    form.confirm_password.clear();
    Ok(view! { setup(form: form, errors: errors) })
}

fn validate(form: &SetupForm) -> SetupErrors {
    let email = form.email.trim();
    SetupErrors {
        first_name: form
            .first_name
            .trim()
            .is_empty()
            .then_some("First name is required"),
        last_name: form
            .last_name
            .trim()
            .is_empty()
            .then_some("Last name is required"),
        email: if email.is_empty() {
            Some("Email is required")
        } else if !looks_like_email(email) {
            Some("Enter a valid email")
        } else {
            None
        },
        password: if form.password.is_empty() {
            Some("Password is required")
        } else if form.password.chars().count() < MIN_PASSWORD_LEN {
            Some("Password must be at least 8 characters")
        } else {
            None
        },
        confirm_password: (form.confirm_password != form.password)
            .then_some("Passwords do not match"),
    }
}

/// Catches typos such as a missing `@` or domain. It does not prove the address exists.
fn looks_like_email(email: &str) -> bool {
    email.split_once('@').is_some_and(|(user, domain)| {
        !user.is_empty()
            && !domain.contains('@')
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && !email.contains(char::is_whitespace)
    })
}

/// The setup page: the form filled with `form`, and `errors` under its fields.
#[component]
async fn setup(form: SetupForm, errors: SetupErrors) -> Result<impl View> {
    Ok(view! {
        onboarding(
            <div class="flex flex-col gap-2">
                <span class="text-xs font-medium tracking-wider text-primary uppercase">
                    "First run"
                </span>
                <h1 class="text-2xl font-semibold tracking-tight">"Set up the server"</h1>
                <p class="text-sm text-muted-foreground">
                    "Create the super admin account. It owns this server and everything deployed on it."
                </p>
            </div>
            <form class="flex flex-col gap-4" method="post">
                <div class="grid gap-4 sm:grid-cols-2">
                    setup_field(
                        label: "First name",
                        error: errors.first_name,
                        attrs: attributes! {
                            name="first_name"
                            value=(&form.first_name)
                            placeholder="John"
                            autocomplete="given-name"
                            required=""
                        }
                    )
                    setup_field(
                        label: "Last name",
                        error: errors.last_name,
                        attrs: attributes! {
                            name="last_name"
                            value=(&form.last_name)
                            placeholder="Doe"
                            autocomplete="family-name"
                            required=""
                        }
                    )
                </div>
                setup_field(
                    label: "Email",
                    error: errors.email,
                    attrs: attributes! {
                        type="email"
                        name="email"
                        value=(&form.email)
                        placeholder="you@example.com"
                        autocomplete="email"
                        required=""
                    }
                )
                setup_field(
                    label: "Password",
                    error: errors.password,
                    password: true,
                    attrs: attributes! {
                        name="password"
                        placeholder="At least 8 characters"
                        autocomplete="new-password"
                        minlength=(MIN_PASSWORD_LEN.to_string())
                        required=""
                    }
                )
                setup_field(
                    label: "Confirm password",
                    error: errors.confirm_password,
                    password: true,
                    attrs: attributes! {
                        name="confirm_password"
                        placeholder="Repeat the password"
                        autocomplete="new-password"
                        required=""
                    }
                )
                button(
                    size: ButtonSize::Lg,
                    attrs: attributes! { type="submit" class="mt-2 w-full" },
                    "Create account"
                    icon(data: iconify_icon!("lucide:arrow-right"), size: 16)
                )
            </form>
        )
    })
}

/// A labelled input of the setup form, with `error` below it. `attrs` go to the input.
#[component]
async fn setup_field(
    cx: &Cx,
    label: &'static str,
    error: Option<&'static str>,
    #[default] password: bool,
    #[default] attrs: Attributes,
) -> Result<impl View> {
    let id = element_id(cx);
    let attrs = attributes! {
        id=(&id)
        aria-invalid=(if error.is_some() { "true" } else { "false" })
        (attrs)
    };

    Ok(view! {
        field(
            field_label(attrs: attributes! { for=(&id) }, (label))
            if password {
                password_input(attrs: attrs)
            } else {
                input(attrs: attrs)
            }
            if let Some(error) = error {
                field_error((error))
            }
        )
    })
}
