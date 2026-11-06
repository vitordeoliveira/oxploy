//! The MCP page: where AI clients, such as a bot or Claude Code, reach this server and
//! its apps over the Model Context Protocol, at `/mcp`.
//!
//! The super admin makes server tokens here, which read the machine, oxploy itself and
//! every app it runs. Each organization makes tokens of its own, which reach the tools
//! of its apps that were turned on in each app's MCP tab.
//!
//! Making a token sends the browser on to this page with an ID, which shows the token
//! once. Reloading the page sends the form no more, so it makes no second token.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{IconData, icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, see_other},
        href, page, query_params, route,
    },
    runtime::{Event, Signal, signal},
    view::{Child, View, attributes, component, element_id, view},
};

use super::projects::created_at;
use super::projects::project_id::services::service_id::ServiceTab;
use super::{panel_url, require_organization, require_server_owner, require_sign_in};
use crate::components::alert_dialog::alert_dialog;
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::dialog::{
    dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title,
};
use crate::components::field::{field, field_description, field_error, field_label};
use crate::components::input::input;
use crate::features::app_mcp::McpTool;
use crate::features::mcp::{IssueError, MAX_NAME_LEN, McpToken, client_name};
use crate::features::project::Project;
use crate::features::service::Service;
use crate::features::user::Role;

/// Whose tokens a part of the page holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// The server's, which only the super admin sees.
    Server,
    /// The organization the browser is working in.
    Organization,
}

impl Kind {
    /// How the forms name it.
    fn code(self) -> &'static str {
        match self {
            Kind::Server => "server",
            Kind::Organization => "organization",
        }
    }

    fn parse(code: &str) -> Option<Kind> {
        [Kind::Server, Kind::Organization]
            .into_iter()
            .find(|kind| kind.code() == code)
    }

    /// The organization the kind's tokens belong to, `None` for the server's, once the
    /// browser is checked to manage them.
    async fn owner(self, cx: &Cx) -> Result<Option<u64>> {
        Ok(match self {
            Kind::Server => {
                require_server_owner(cx).await?;
                None
            }
            Kind::Organization => Some(require_organization(cx).await?.id),
        })
    }
}

/// A token just made, which the page shows this once.
struct Issued {
    kind: Kind,
    name: String,
    secret: String,
    /// The name the client knows the token's server by, for the command that adds it.
    client: String,
}

/// A name the form turned down, shown again in its dialog with why.
struct Refused {
    kind: Kind,
    name: String,
    problem: IssueError,
}

/// What a form left for the page to show once, after it sent the browser back.
enum Shown {
    Issued(Issued),
    Refused(Refused),
}

/// How long what a form left waits for the page, which the browser opens right away.
const SHOWN_FOR: Duration = Duration::from_secs(5 * 60);

/// What a form left for one user's next view of the page.
struct Waiting {
    user_id: u64,
    shown: Shown,
    at: Instant,
}

/// What the forms left for the page, by the ID they sent the browser back with. Kept in
/// memory only, as a token is shown once and never stored.
static WAITING: LazyLock<Mutex<HashMap<String, Waiting>>> = LazyLock::new(Mutex::default);

/// Leaves `shown` for `user_id`'s next view of the page, and returns its ID.
fn leave(user_id: u64, shown: Shown) -> String {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).expect("the system provides random bytes");
    let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut waiting = WAITING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    waiting.retain(|_, left| left.at.elapsed() < SHOWN_FOR);
    waiting.insert(
        id.clone(),
        Waiting {
            user_id,
            shown,
            at: Instant::now(),
        },
    );
    id
}

/// Takes what was left with `id` for `user_id`, if it is still there.
fn take(user_id: u64, id: &str) -> Option<Shown> {
    let mut waiting = WAITING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let left = waiting.get(id)?;
    if left.user_id != user_id || left.at.elapsed() >= SHOWN_FOR {
        return None;
    }
    waiting.remove(id).map(|left| left.shown)
}

/// The page's address, with what a form left in `shown`.
#[query_params]
struct PageQuery {
    shown: Option<String>,
}

/// The MCP page, with the token a form just made or the name it turned down.
#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    let signed_in = require_sign_in(cx).await?;
    let shown = query_params::<PageQuery>(cx)
        .ok()
        .and_then(|query| query.shown.clone())
        .and_then(|id| take(signed_in.user.id, &id));

    Ok(view! { mcp_view(shown: shown.as_ref()) })
}

/// The form that makes a token.
#[derive(Deserialize)]
struct IssueForm {
    kind: String,
    name: String,
}

/// Makes a token, and sends the browser on to the page, which shows it once: only its
/// hash is kept. A name it turns down goes back to the dialog with why.
#[route(POST "./issue")]
async fn issue(cx: &Cx, Form(form): Form<IssueForm>) -> Result<SeeOther> {
    let kind = Kind::parse(&form.kind).ok_or_bad_request("No such kind of token")?;
    let signed_in = require_sign_in(cx).await?;
    let owner = kind.owner(cx).await?;
    let organization = match kind {
        Kind::Server => None,
        Kind::Organization => Some(require_organization(cx).await?.name),
    };
    let shown = match McpToken::issue(cx, owner, &form.name).await? {
        Ok((token, secret)) => Shown::Issued(Issued {
            kind,
            client: client_name(organization.as_deref(), &token.name),
            name: token.name,
            secret,
        }),
        Err(problem) => Shown::Refused(Refused {
            kind,
            name: form.name,
            problem,
        }),
    };
    let id = leave(signed_in.user.id, shown);
    Ok(see_other(format!("{}?shown={id}", href!(page).resolve(cx))))
}

/// The form that revokes a token.
#[derive(Deserialize)]
struct RevokeForm {
    kind: String,
    id: u64,
}

/// Revokes a token: clients that send it are turned away from then on.
#[route(POST "./revoke")]
async fn revoke(cx: &Cx, Form(form): Form<RevokeForm>) -> Result<SeeOther> {
    let kind = Kind::parse(&form.kind).ok_or_bad_request("No such kind of token")?;
    let owner = kind.owner(cx).await?;
    McpToken::find_of(cx, owner, form.id)
        .await?
        .ok_or_not_found()?
        .revoke(cx)
        .await?;
    Ok(see_other(href!(page).resolve(cx)))
}

/// The page, with the token a form just made, or the name it just turned down.
#[component]
async fn mcp_view(cx: &Cx, shown: Option<&Shown>) -> Result<impl View> {
    let (issued, refused) = match shown {
        Some(Shown::Issued(issued)) => (Some(issued), None),
        Some(Shown::Refused(refused)) => (None, Some(refused)),
        None => (None, None),
    };
    let signed_in = require_sign_in(cx).await?;
    let owns_server = signed_in.user.role == Role::SuperAdmin;
    let organization = require_organization(cx).await?;
    let endpoint = format!("{}/mcp", panel_url(cx).unwrap_or_default());
    let issued_server = issued.filter(|issued| issued.kind == Kind::Server);
    let issued_organization = issued.filter(|issued| issued.kind == Kind::Organization);
    let refused_server = refused.filter(|refused| refused.kind == Kind::Server);
    let refused_organization = refused.filter(|refused| refused.kind == Kind::Organization);

    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            <div class="flex flex-col gap-1">
                <h1 class="text-2xl font-semibold tracking-tight">"MCP"</h1>
                <p class="text-sm text-muted-foreground">
                    "Where AI clients, such as a bot or Claude Code, reach this server and its apps."
                </p>
            </div>
            <section class="flex flex-col gap-2 rounded-xl border border-border bg-card p-5">
                <h2 class="font-semibold">"Endpoint"</h2>
                <code class="w-fit max-w-full rounded-md bg-muted px-2 py-1 font-mono text-sm break-all select-all">
                    (&endpoint)
                </code>
                <p class="text-sm text-muted-foreground">
                    "Clients send a token as "
                    <code class="font-mono text-foreground">"Authorization: Bearer <token>"</code>
                    ". What they reach depends on the token: the server, or one organization's apps."
                </p>
            </section>
            if owns_server {
                tokens_section(
                    kind: Kind::Server,
                    title: "Server",
                    text: "Reads the machine, oxploy itself, and the usage, logs and deployments of every app on it. Only you see this, as the server's owner, and it is the same in every organization. Its tools only read.",
                    symbol: iconify_icon!("lucide:server"),
                    endpoint: &endpoint,
                    issued: issued_server,
                    refused: refused_server
                )
            }
            tokens_section(
                kind: Kind::Organization,
                title: &organization.name,
                text: "Reaches the tools of this organization's apps that are turned on in each app's MCP tab. Other organizations have tokens of their own.",
                symbol: iconify_icon!("lucide:boxes"),
                endpoint: &endpoint,
                issued: issued_organization,
                refused: refused_organization,
                apps_list(organization_id: organization.id)
            )
        </div>
    })
}

/// One kind of tokens: the token just made, what `child` adds, and the tokens to revoke.
#[component]
async fn tokens_section(
    cx: &Cx,
    kind: Kind,
    title: &str,
    text: &str,
    symbol: IconData,
    endpoint: &str,
    issued: Option<&Issued>,
    refused: Option<&Refused>,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    let owner = match kind {
        Kind::Server => None,
        Kind::Organization => Some(require_organization(cx).await?.id),
    };
    let tokens = McpToken::of(cx, owner).await?;
    // A name turned down opens the dialog again, to be changed.
    let issuing = signal(cx, || refused.is_some());
    let open_issuing = issuing.clone();
    let revoking: Vec<_> = tokens
        .iter()
        .map(|token| signal(&cx.keyed(token.id), || false))
        .collect();

    Ok(view! {
        <section class="flex flex-col rounded-xl border border-border bg-card">
            <header class="flex flex-wrap items-center gap-3 border-b border-border p-5">
                <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-foreground/5">
                    icon(data: symbol, size: 18)
                </span>
                <div class="flex min-w-56 flex-1 flex-col">
                    <h2 class="font-semibold">(title)</h2>
                    <p class="text-sm text-muted-foreground">(text)</p>
                </div>
                button(
                    attrs: attributes! { type="button" @click=$(|_e: Event| open_issuing.set(true)) },
                    icon(data: iconify_icon!("lucide:plus"))
                    "New token"
                )
            </header>
            if let Some(issued) = issued {
                issued_token(issued: issued, endpoint: endpoint)
            }
            (child)
            if tokens.is_empty() {
                <p class="p-5 text-sm text-muted-foreground">
                    "No tokens yet. Make one for each client, so you can revoke one without the others."
                </p>
            } else {
                <ul class="divide-y divide-border">
                    #[key(token.id)]
                    for (index, token) in tokens.iter().enumerate() {
                        token_row(token: token, revoking: &revoking[index])
                    }
                </ul>
            }
        </section>
        issue_dialog(open: &issuing, kind: kind, title: title, refused: refused)
        #[key(token.id)]
        for (index, token) in tokens.iter().enumerate() {
            revoke_dialog(open: &revoking[index], kind: kind, token: token)
        }
    })
}

/// The organization's apps that serve MCP, with how many of their tools are on, each
/// linked to its MCP tab.
#[component]
async fn apps_list(cx: &Cx, organization_id: u64) -> Result<impl View> {
    let mut apps = Vec::new();
    for project in Project::in_organization(cx, organization_id).await? {
        for service in Service::in_project(cx, project.id).await? {
            if service.mcp_path.is_none() {
                continue;
            }
            let on = McpTool::enabled_of(cx, service.id).await?.len();
            let url = ServiceTab::Mcp.url(cx, &project, &service);
            apps.push((project.name.clone(), service, on, url));
        }
    }

    Ok(view! {
        <div class="flex flex-col gap-2 border-b border-border p-5">
            <h3 class="text-sm font-medium">"Apps"</h3>
            if apps.is_empty() {
                <p class="text-sm text-muted-foreground">
                    "No app of this organization serves MCP yet. Turn it on in an app's MCP tab, then turn on the tools it passes on."
                </p>
            } else {
                <ul class="flex flex-col gap-1.5">
                    #[key(service.id)]
                    for (project, service, on, url) in &apps {
                        <li class="flex flex-wrap items-baseline gap-x-2 text-sm">
                            <a href=(url) class="font-medium underline-offset-4 hover:underline">
                                (project) " / " (&service.name)
                            </a>
                            <span class="font-mono text-xs text-muted-foreground">(&service.app_name)</span>
                            <span class="text-muted-foreground">
                                "· "
                                (match on {
                                    0 => "no tools on".to_owned(),
                                    1 => "1 tool on".to_owned(),
                                    on => format!("{on} tools on"),
                                })
                            </span>
                        </li>
                    }
                </ul>
            }
        </div>
    })
}

/// The token just made, with the command that connects Claude Code to it.
#[component]
async fn issued_token(issued: &Issued, endpoint: &str) -> Result<impl View> {
    let command = format!(
        "claude mcp add --transport http {} {endpoint} --header \"Authorization: Bearer {}\"",
        issued.client, issued.secret
    );

    Ok(view! {
        <div class="flex flex-col gap-3 border-b border-border bg-primary/5 p-5">
            <div class="flex items-start gap-2">
                icon(data: iconify_icon!("lucide:key-round"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0 text-primary" })
                <p class="text-sm">
                    "Copy the token for " <strong class="font-medium">(&issued.name)</strong>
                    " now. oxploy keeps only its hash, so it cannot show it again."
                </p>
            </div>
            <code class="rounded-md border border-border bg-background px-3 py-2 font-mono text-sm break-all select-all">
                (&issued.secret)
            </code>
            <p class="text-sm text-muted-foreground">"To connect Claude Code:"</p>
            <code class="rounded-md border border-border bg-background px-3 py-2 font-mono text-xs break-all whitespace-pre-wrap select-all">
                (command)
            </code>
        </div>
    })
}

/// One token: its name, when it was made and last used, and a button to revoke it.
#[component]
async fn token_row(token: &McpToken, revoking: &Signal<bool>) -> Result<impl View> {
    let revoking = revoking.clone();
    let label = format!("Revoke {}", token.name);

    Ok(view! {
        <li class="flex items-center gap-3 px-5 py-4">
            <div class="flex min-w-0 flex-1 flex-col gap-0.5">
                <span class="truncate font-medium">(&token.name)</span>
                <span class="text-xs text-muted-foreground">
                    "Created " created_at(seconds: token.created_at) " · "
                    if let Some(used) = token.last_used_at {
                        "Last used " created_at(seconds: used)
                    } else {
                        "Never used"
                    }
                </span>
            </div>
            button(
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Icon,
                attrs: attributes! {
                    type="button"
                    class="hover:text-destructive"
                    title="Revoke"
                    aria-label=(&label)
                    @click=$(|_e: Event| revoking.set(true))
                },
                icon(data: iconify_icon!("lucide:trash-2"))
            )
        </li>
    })
}

/// A dialog that names a new token of `kind`, for `title`: the server, or the
/// organization's name. A name just turned down comes back in it, with why.
#[component]
async fn issue_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    kind: Kind,
    title: &str,
    refused: Option<&Refused>,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let name_id = element_id(cx);
    let hint_id = element_id(cx);
    let problem = refused.map(|refused| match refused.problem {
        IssueError::BadName => format!(
            "Give it a name with letters or digits, of up to {MAX_NAME_LEN} characters."
        ),
        IssueError::Taken => format!(
            "{} already has a token named like {}. Pick another name, so each one's client gets a name of its own.",
            match kind {
                Kind::Server => "The server",
                Kind::Organization => title,
            },
            refused.name.trim(),
        ),
    });
    let (heading, about) = match kind {
        Kind::Server => (
            "New server token".to_owned(),
            "A token for one client, such as a bot. It reads the server and every app on it, and is shown only once.",
        ),
        Kind::Organization => (
            format!("New token for {title}"),
            "A token for one client, such as a bot. It reaches the tools of this organization's apps that are turned on, and is shown only once.",
        ),
    };

    Ok(view! {
        dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, (heading))
                    dialog_description((about))
                )
                <form method="post" action=(href!(issue)) class="flex flex-col gap-4">
                    <input type="hidden" name="kind" value=(kind.code())>
                    field(
                        field_label(attrs: attributes! { for=(&name_id) }, "Name")
                        input(
                            attrs: attributes! {
                                id=(&name_id)
                                name="name"
                                value=(refused.map_or("", |refused| refused.name.as_str()))
                                placeholder="Telegram bot"
                                maxlength=(MAX_NAME_LEN.to_string())
                                required=""
                                autocomplete="off"
                                aria-describedby=(&hint_id)
                                aria-invalid=(problem.is_some().then_some("true"))
                            }
                        )
                        if let Some(problem) = &problem {
                            field_error(attrs: attributes! { id=(&hint_id) }, (problem))
                        } else {
                            field_description(
                                attrs: attributes! { id=(&hint_id) },
                                "What uses it, so you know which one to revoke. Its client gets a name made from it."
                            )
                        }
                    )
                    dialog_footer(
                        button(
                            variant: ButtonVariant::Ghost,
                            attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                            "Cancel"
                        )
                        button(attrs: attributes! { type="submit" }, "Create token")
                    )
                </form>
            )
        )
    })
}

/// A dialog that confirms revoking `token`.
#[component]
async fn revoke_dialog(cx: &Cx, open: &Signal<bool>, kind: Kind, token: &McpToken) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let description_id = element_id(cx);
    let reach = match kind {
        Kind::Server => " will no longer reach the server.",
        Kind::Organization => " will no longer reach this organization's apps.",
    };

    Ok(view! {
        alert_dialog(
            open: $(open.get()),
            attrs: attributes! {
                aria-labelledby=(&title_id)
                aria-describedby=(&description_id)
            },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Revoke token")
                    dialog_description(
                        attrs: attributes! { id=(&description_id) },
                        "Clients that use "
                        <strong class="font-medium text-foreground">(&token.name)</strong>
                        (reach)
                        " A revoked token cannot be brought back."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(href!(revoke))>
                        <input type="hidden" name="kind" value=(kind.code())>
                        <input type="hidden" name="id" value=(token.id.to_string())>
                        button(
                            variant: ButtonVariant::Destructive,
                            attrs: attributes! { type="submit" },
                            "Revoke"
                        )
                    </form>
                )
            )
        )
    })
}
