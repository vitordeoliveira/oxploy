//! The Git accounts the active organization deploys from. GitHub is the only provider
//! so far.
//!
//! Connecting an account registers a GitHub App on it: oxploy sends the browser to
//! GitHub with the app's manifest, GitHub sends it back once the user confirms, and
//! again once they install the app.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        href, page, query_params,
        route,
    },
    runtime::{Event, Signal, signal},
    view::{View, attributes, component, element_id, view},
};

use super::{panel_url, require_organization};
use crate::components::alert_dialog::alert_dialog;
use crate::components::button::{ButtonSize, ButtonVariant, button, button_variants};
use crate::components::dialog::{
    dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title,
};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::features::github::{GithubApp, Owner, RegistrationError};

/// The organization's GitHub accounts.
#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    let organization = require_organization(cx).await?;
    let mut apps = GithubApp::in_organization(cx, organization.id).await?;
    // An app installed without coming back to oxploy, such as from GitHub's own pages,
    // shows up as installed here.
    let mut problems = Vec::new();
    for app in &mut apps {
        let problem = match app.installation_id {
            Some(_) => None,
            None => app.refresh_installation(cx).await?.err(),
        };
        problems.push(problem.map(|problem| problem.to_string()));
    }
    let connecting = signal(cx, || false);
    let removing: Vec<_> = apps
        .iter()
        .map(|app| signal(&cx.keyed(app.id), || false))
        .collect();
    let open_connecting = connecting.clone();

    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            <div class="flex flex-wrap items-end justify-between gap-4">
                <div class="flex flex-col gap-1">
                    <h1 class="text-2xl font-semibold tracking-tight">"Git"</h1>
                    <p class="text-sm text-muted-foreground">
                        "The accounts oxploy reads your code from."
                    </p>
                </div>
                if !apps.is_empty() {
                    button(
                        attrs: attributes! { type="button" @click=$(|_e: Event| open_connecting.set(true)) },
                        icon(data: iconify_icon!("lucide:plus"))
                        "Connect GitHub"
                    )
                }
            </div>
            <section class="flex flex-col rounded-xl border border-border bg-card">
                <header class="flex items-center gap-3 border-b border-border p-5">
                    <span class="flex size-9 items-center justify-center rounded-lg bg-foreground/5">
                        icon(data: iconify_icon!("lucide:github"), size: 18)
                    </span>
                    <div class="flex flex-col">
                        <h2 class="font-semibold">"GitHub"</h2>
                        <p class="text-sm text-muted-foreground">
                            "Each account gets a GitHub App of its own, which can only read the repositories you choose."
                        </p>
                    </div>
                </header>
                if apps.is_empty() {
                    <div class="flex flex-col items-center gap-3 p-10 text-center">
                        <p class="max-w-md text-sm text-muted-foreground">
                            "Connect a personal account or an organization. GitHub asks you to confirm the app and pick its repositories, then sends you back here."
                        </p>
                        button(
                            attrs: attributes! { type="button" @click=$(|_e: Event| connecting.set(true)) },
                            icon(data: iconify_icon!("lucide:github"))
                            "Connect GitHub"
                        )
                    </div>
                } else {
                    <ul class="divide-y divide-border">
                        #[key(app.id)]
                        for (index, app) in apps.iter().enumerate() {
                            app_row(app: app, problem: problems[index].as_deref(), removing: &removing[index])
                        }
                    </ul>
                }
            </section>
        </div>
        connect_dialog(open: &connecting)
        #[key(app.id)]
        for (index, app) in apps.iter().enumerate() {
            remove_dialog(open: &removing[index], app: app)
        }
    })
}

/// One GitHub App: the account it reads from, or a button to install it.
#[component]
async fn app_row(app: &GithubApp, problem: Option<&str>, removing: &Signal<bool>) -> Result<impl View> {
    let removing = removing.clone();

    Ok(view! {
        <li class="flex flex-wrap items-center gap-3 px-5 py-4">
            <div class="flex min-w-0 flex-1 flex-col gap-0.5">
                if let Some(account) = &app.account {
                    <span class="truncate font-medium">(account)</span>
                } else {
                    <span class="truncate font-medium text-muted-foreground">"Not installed yet"</span>
                }
                <a
                    href=(&app.html_url)
                    target="_blank"
                    rel="noreferrer"
                    class="inline-flex w-fit items-center gap-1 font-mono text-xs text-muted-foreground hover:text-foreground"
                >
                    (&app.name)
                    icon(data: iconify_icon!("lucide:external-link"), size: 12)
                </a>
                if let Some(problem) = problem {
                    <span class="text-xs text-destructive">(problem)</span>
                }
            </div>
            if app.installation_id.is_some() {
                <span class="inline-flex items-center gap-1.5 rounded-full border border-border px-2 py-0.5 text-xs text-muted-foreground">
                    icon(data: iconify_icon!("lucide:circle-check"), size: 12, attrs: attributes! { class="text-primary" })
                    "Installed"
                </span>
            } else {
                <a href=(app.install_url()) class=(button_variants(ButtonVariant::Primary, ButtonSize::Sm))>
                    "Install on GitHub"
                </a>
            }
            button(
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Icon,
                attrs: attributes! {
                    type="button"
                    class="hover:text-destructive"
                    title="Remove"
                    aria-label="Remove"
                    @click=$(|_e: Event| removing.set(true))
                },
                icon(data: iconify_icon!("lucide:trash-2"))
            )
        </li>
    })
}

/// A dialog that asks which GitHub account to connect.
#[component]
async fn connect_dialog(cx: &Cx, open: &Signal<bool>) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let organization_id = element_id(cx);

    Ok(view! {
        dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Connect GitHub")
                    dialog_description(
                        "oxploy registers a GitHub App with read access to code. You confirm it on GitHub, then choose its repositories."
                    )
                )
                <form method="post" action=(href!(connect)) class="flex flex-col gap-4">
                    field(
                        field_label(attrs: attributes! { for=(&organization_id) }, "GitHub organization")
                        input(
                            attrs: attributes! {
                                id=(&organization_id)
                                name="organization"
                                placeholder="Optional"
                                maxlength="39"
                                pattern="[A-Za-z0-9][A-Za-z0-9\\-]*"
                                autocomplete="off"
                            }
                        )
                        field_description(
                            "Leave it empty to connect your personal account. For an organization, you need to be one of its owners."
                        )
                    )
                    dialog_footer(
                        button(
                            variant: ButtonVariant::Ghost,
                            attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                            "Cancel"
                        )
                        button(attrs: attributes! { type="submit" }, "Continue")
                    )
                </form>
            )
        )
    })
}

/// A dialog that confirms removing `app` from oxploy.
#[component]
async fn remove_dialog(cx: &Cx, open: &Signal<bool>, app: &GithubApp) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let description_id = element_id(cx);

    Ok(view! {
        alert_dialog(
            open: $(open.get()),
            attrs: attributes! {
                aria-labelledby=(&title_id)
                aria-describedby=(&description_id)
            },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Remove GitHub account")
                    dialog_description(
                        attrs: attributes! { id=(&description_id) },
                        "Services that build from "
                        <strong class="font-medium text-foreground">
                            (app.account.as_deref().unwrap_or(&app.name))
                        </strong>
                        " will need another account. The app stays on GitHub until you delete it from its settings there."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(href!(remove))>
                        <input type="hidden" name="id" value=(app.id.to_string())>
                        button(
                            variant: ButtonVariant::Destructive,
                            attrs: attributes! { type="submit" },
                            "Remove"
                        )
                    </form>
                )
            )
        )
    })
}

/// A form that starts connecting a GitHub account.
#[derive(Deserialize)]
struct ConnectForm {
    #[serde(default)]
    organization: String,
}

/// Shows what GitHub will ask to confirm, with the button that sends the app's
/// manifest there.
#[page(POST "./github")]
async fn connect(cx: &Cx, Form(form): Form<ConnectForm>) -> Result<impl View> {
    let organization = require_organization(cx).await?;
    let login = form.organization.trim();
    let owner = match login {
        "" => Owner::Personal,
        login => Owner::Organization(login),
    };
    let panel = panel_url(cx).ok_or_bad_request("The request does not name a host")?;
    let registration = GithubApp::start_registration(
        organization.id,
        &owner,
        &panel,
        &format!("{panel}{}", href!(created).resolve(cx)),
        &format!("{panel}{}", href!(installed).resolve(cx)),
    )
    .ok_or_bad_request("That is not a GitHub organization name")?;
    let account = match owner {
        Owner::Personal => "your personal account".to_owned(),
        Owner::Organization(login) => format!("the {login} organization"),
    };

    Ok(view! {
        <div class="flex flex-1 items-start justify-center pt-10">
            <section class="flex w-full max-w-lg flex-col gap-5 rounded-xl border border-border bg-card p-6">
                <span class="flex size-11 items-center justify-center rounded-xl bg-foreground/5">
                    icon(data: iconify_icon!("lucide:github"), size: 22)
                </span>
                <div class="flex flex-col gap-2">
                    <h1 class="text-xl font-semibold tracking-tight">"Register the app on GitHub"</h1>
                    <p class="text-sm text-muted-foreground">
                        "GitHub will ask you to create a private app on " (account)
                        ". It can read code and nothing else. Next, you install it and pick the repositories oxploy may build."
                    </p>
                </div>
                <form method="post" action=(&registration.action) class="flex justify-end gap-2">
                    <input type="hidden" name="manifest" value=(&registration.manifest)>
                    <a href=(href!(page)) class=(button_variants(ButtonVariant::Ghost, ButtonSize::Md))>"Cancel"</a>
                    button(
                        attrs: attributes! { type="submit" },
                        "Continue to GitHub"
                        icon(data: iconify_icon!("lucide:external-link"))
                    )
                </form>
            </section>
        </div>
    })
}

/// What GitHub sends the browser back with once the user confirms the app.
#[query_params(error = bad_request)]
struct Created {
    code: Option<String>,
    state: Option<String>,
}

/// Saves the app GitHub registered, then sends the browser to install it.
#[page("./github/created")]
async fn created(cx: &Cx) -> Result<impl View> {
    let organization = require_organization(cx).await?;
    let query = query_params::<Created>(cx)?;
    let (Some(code), Some(state)) = (&query.code, &query.state) else {
        return Err(bad_request("GitHub did not send the app back").into());
    };
    let problem = match GithubApp::finish_registration(cx, organization.id, state, code).await? {
        Ok(app) => return Err(see_other(app.install_url()).into()),
        Err(RegistrationError::Expired) => {
            "This connection was started elsewhere or took longer than an hour. Start it again from here.".to_owned()
        }
        Err(RegistrationError::Github(error)) => error.to_string(),
    };

    Ok(view! {
        <div class="flex flex-1 items-start justify-center pt-10">
            <section class="flex w-full max-w-lg flex-col gap-4 rounded-xl border border-border bg-card p-6">
                <span class="flex size-11 items-center justify-center rounded-xl bg-destructive/10 text-destructive">
                    icon(data: iconify_icon!("lucide:triangle-alert"), size: 22)
                </span>
                <div class="flex flex-col gap-2">
                    <h1 class="text-xl font-semibold tracking-tight">"GitHub was not connected"</h1>
                    <p class="text-sm text-muted-foreground">(problem)</p>
                </div>
                <div class="flex justify-end">
                    <a href=(href!(page)) class=(button_variants(ButtonVariant::Primary, ButtonSize::Md))>"Back to Git"</a>
                </div>
            </section>
        </div>
    })
}

/// Where GitHub sends the browser once the user installs an app: the Git page, which
/// looks the installation up.
#[page("./github/installed")]
async fn installed(cx: &Cx) -> Result<()> {
    require_organization(cx).await?;
    Err(see_other(href!(page).resolve(cx)).into())
}

/// A form that acts on one GitHub App.
#[derive(Deserialize)]
struct AppForm {
    id: u64,
}

/// Removes a GitHub App from the organization.
#[route(POST "./github/remove")]
async fn remove(cx: &Cx, Form(form): Form<AppForm>) -> Result<SeeOther> {
    let organization = require_organization(cx).await?;
    let app = GithubApp::find_in(cx, organization.id, form.id)
        .await?
        .ok_or_not_found()?;
    app.remove(cx).await?;
    Ok(see_other(href!(page).resolve(cx)))
}
