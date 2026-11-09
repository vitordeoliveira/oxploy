//! The MCP page: where AI clients, such as a bot or Claude Code, reach this server and
//! its apps over the Model Context Protocol, at `/mcp`.
//!
//! The super admin makes server tokens here, which read the machine, oxploy itself and
//! every app it runs. Each organization makes tokens of its own, which reach the tools
//! of its apps that were turned on in each app's MCP tab: all of them, or only the ones
//! picked for the token, which a new token starts with.
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
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        href, page, query_params, route,
    },
    runtime::{Event, Signal, procedure, signal},
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
use crate::components::switch::switch;
use crate::db::db;
use crate::features::app_mcp::{self, Effect, McpTool};
use crate::features::mcp::{ClaudeScope, IssueError, MAX_NAME_LEN, McpToken, claude_code_command, client_name};
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

/// A name the form turned down, shown again in its dialog with why, and with the tools
/// it picked.
struct Refused {
    kind: Kind,
    name: String,
    problem: IssueError,
    /// The tools picked, or `None` for every tool turned on.
    picked: Option<Vec<(u64, String)>>,
    /// Whether a server token was to manage.
    manages: bool,
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

/// Makes a token, and sends the browser on to the page, which shows it once: only its
/// hash is kept. A name it turns down goes back to the dialog with why.
///
/// The form sends `kind` and `name`. A server token also manages when it sends
/// `manages`. An organization's token reaches every tool turned on when it sends `all`,
/// and otherwise only the tools it sends as `tool`, each as `<service_id>:<name>`, of
/// those turned on in the organization's apps.
#[route(POST "./issue")]
async fn issue(cx: &Cx, Form(fields): Form<Vec<(String, String)>>) -> Result<SeeOther> {
    let value_of = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map_or("", |(_, value)| value.as_str())
    };
    let kind = Kind::parse(value_of("kind")).ok_or_bad_request("No such kind of token")?;
    let name = value_of("name").to_owned();
    let signed_in = require_sign_in(cx).await?;
    let owner = kind.owner(cx).await?;
    let (organization, picked) = match kind {
        Kind::Server => (None, None),
        Kind::Organization => {
            let organization = require_organization(cx).await?;
            let picked = if fields.iter().any(|(name, _)| name == "all") {
                None
            } else {
                let offered = app_mcp::serving(cx, organization.id).await?;
                let picked: Vec<_> = fields
                    .iter()
                    .filter(|(name, _)| name == "tool")
                    .filter_map(|(_, value)| tool_choice(value))
                    .filter(|(service_id, name)| {
                        offered
                            .iter()
                            .any(|app| app.service.id == *service_id && app.enabled.contains(name))
                    })
                    .collect();
                Some(picked)
            };
            (Some(organization.name), picked)
        }
    };
    let manages = kind == Kind::Server && fields.iter().any(|(name, _)| name == "manages");
    let shown = match McpToken::issue(cx, owner, &name, picked.as_deref(), manages).await? {
        Ok((token, secret)) => Shown::Issued(Issued {
            kind,
            client: client_name(organization.as_deref(), &token.name),
            name: token.name,
            secret,
        }),
        Err(problem) => Shown::Refused(Refused {
            kind,
            name,
            problem,
            picked,
            manages,
        }),
    };
    let id = leave(signed_in.user.id, shown);
    Ok(see_other(format!("{}?shown={id}", href!(page).resolve(cx))))
}

/// A tool as the forms send it, `<service_id>:<name>`, split into the two.
fn tool_choice(value: &str) -> Option<(u64, String)> {
    let (service_id, name) = value.split_once(':')?;
    Some((service_id.parse().ok()?, name.to_owned())).filter(|(_, name)| !name.is_empty())
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

/// Has an organization's token reach every tool turned on, or only the ones picked for
/// it, from the switch in its tools dialog, and returns whether it reaches every one
/// now. It answers requests of its own, so it checks that the token is of the
/// organization the browser works in.
#[procedure]
async fn reach_all(cx: &Cx, token_id: u64, all: bool) -> Result<bool> {
    let organization = require_organization(cx).await?;
    let mut token = McpToken::find_of(cx, Some(organization.id), token_id)
        .await?
        .ok_or_not_found()?;
    token.set_picks_tools(cx, !all).await?;
    Ok(all)
}

/// Picks one of an app's tools for an organization's token, or leaves it out, from the
/// switch in its row of the token's tools dialog, and returns whether it is picked now.
/// It answers requests of its own, so it checks that the token and the app are both of
/// the organization the browser works in.
#[procedure]
async fn pick_tool(cx: &Cx, token_id: u64, service_id: u64, name: String, on: bool) -> Result<bool> {
    let organization = require_organization(cx).await?;
    let token = McpToken::find_of(cx, Some(organization.id), token_id)
        .await?
        .ok_or_not_found()?;
    let service = Service::filter_by_id(service_id)
        .first()
        .exec(&mut db(cx))
        .await?
        .ok_or_not_found()?;
    Project::find_in(cx, organization.id, service.project_id)
        .await?
        .ok_or_not_found()?;
    if !token.pick(cx, service.id, &name, on).await? {
        return Err(bad_request("That is not a tool's name").into());
    }
    Ok(on)
}

/// A tool turned on in one of the organization's apps, which its tokens are picked from.
struct Offered {
    service_id: u64,
    name: String,
    /// What it does, as the app describes it, when the app's server could be asked.
    about: Option<String>,
    /// What it does to the app's data, when the app's server could be asked.
    effect: Option<Effect>,
}

/// One of the organization's apps with tools turned on, and those tools.
struct OfferingApp {
    /// The project and the service, as `Project / Service`.
    label: String,
    tools: Vec<Offered>,
}

/// The tools turned on in the organization's apps, by app, each described as its app's
/// server lists it now. An app that is not running, or cannot be reached, still offers
/// its tools, only without what they do.
async fn offered_tools(cx: &Cx, organization_id: u64) -> Result<Vec<OfferingApp>> {
    let apps = app_mcp::serving(cx, organization_id).await?;
    let mut offered = Vec::new();
    for (app, listing) in app_mcp::list_each(cx, apps).await {
        let listed = match &listing {
            Some(Ok(listing)) => listing.tools.as_slice(),
            _ => &[],
        };
        let mut tools: Vec<_> = app
            .enabled
            .iter()
            .map(|name| {
                let listed = listed.iter().find(|tool| &tool.name == name);
                Offered {
                    service_id: app.service.id,
                    name: name.clone(),
                    about: listed.and_then(|tool| tool.description.clone().or_else(|| tool.title.clone())),
                    effect: listed.map(|tool| tool.effect),
                }
            })
            .collect();
        // In the order the app lists them, then the ones it no longer lists by name.
        tools.sort_by_key(|tool| {
            let at = listed.iter().position(|listed| listed.name == tool.name);
            (at.unwrap_or(usize::MAX), tool.name.clone())
        });
        offered.push(OfferingApp {
            label: format!("{} / {}", app.project, app.service.name),
            tools,
        });
    }
    Ok(offered)
}

/// How many of the tools `offered` are in `picked`.
fn picked_on(offered: &[OfferingApp], picked: &[(u64, String)]) -> usize {
    offered
        .iter()
        .flat_map(|app| &app.tools)
        .filter(|tool| is_picked(picked, tool))
        .count()
}

/// Whether `tool` is in `picked`.
fn is_picked(picked: &[(u64, String)], tool: &Offered) -> bool {
    picked
        .iter()
        .any(|(service_id, name)| *service_id == tool.service_id && *name == tool.name)
}

/// What an organization's token reaches, as its row and its tools dialog show and
/// change it.
#[derive(Clone)]
struct Reaching {
    /// Whether it reaches every tool turned on.
    all: Signal<bool>,
    /// How many of the tools turned on are picked for it.
    picked: Signal<usize>,
    /// Whether its tools dialog is open.
    choosing: Signal<bool>,
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
    let offered = offered_tools(cx, organization.id).await?;
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
                    text: "Reads the machine, oxploy itself, and the usage, logs and deployments of every app on it. A token made to manage also installs oxploy's updates and manages the organizations' tokens and the tools turned on in their apps. Only you see this, as the server's owner, and it is the same in every organization.",
                    symbol: iconify_icon!("lucide:server"),
                    endpoint: &endpoint,
                    issued: issued_server,
                    refused: refused_server,
                    offered: None
                )
            }
            tokens_section(
                kind: Kind::Organization,
                title: &organization.name,
                text: "Reaches the tools of this organization's apps that are turned on in each app's MCP tab: all of them, or only the ones picked for the token. Other organizations have tokens of their own.",
                symbol: iconify_icon!("lucide:boxes"),
                endpoint: &endpoint,
                issued: issued_organization,
                refused: refused_organization,
                offered: Some(&offered),
                apps_list(organization_id: organization.id)
            )
        </div>
    })
}

/// One kind of tokens: the token just made, what `child` adds, and the tokens to revoke.
/// An organization's tokens come with the tools `offered` in its apps, which each one
/// reaches all of or some of.
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
    offered: Option<&[OfferingApp]>,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    let owner = match kind {
        Kind::Server => None,
        Kind::Organization => Some(require_organization(cx).await?.id),
    };
    let tokens = McpToken::of(cx, owner).await?;
    let mut picked = Vec::new();
    for token in &tokens {
        picked.push(match offered {
            Some(_) => token.picked(cx).await?,
            None => Vec::new(),
        });
    }
    let total = offered.map_or(0, |apps| apps.iter().map(|app| app.tools.len()).sum());
    // A name turned down opens the dialog again, to be changed.
    let issuing = signal(cx, || refused.is_some());
    let open_issuing = issuing.clone();
    let revoking: Vec<_> = tokens
        .iter()
        .map(|token| signal(&cx.keyed(token.id), || false))
        .collect();
    let reaching: Vec<_> = tokens
        .iter()
        .zip(&picked)
        .map(|(token, picked)| {
            let offered = offered?;
            Some(Reaching {
                all: signal(&cx.keyed(token.id), || !token.picks_tools()),
                picked: signal(&cx.keyed(token.id), || picked_on(offered, picked)),
                choosing: signal(&cx.keyed(token.id), || false),
            })
        })
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
                        token_row(token: token, revoking: &revoking[index], reaching: reaching[index].as_ref(), total: total)
                    }
                </ul>
            }
        </section>
        issue_dialog(open: &issuing, kind: kind, title: title, refused: refused, offered: offered)
        #[key(token.id)]
        for (index, token) in tokens.iter().enumerate() {
            revoke_dialog(open: &revoking[index], kind: kind, token: token)
            if let (Some(reaching), Some(offered)) = (&reaching[index], offered) {
                tools_dialog(token: token, reaching: reaching, offered: offered, picked: &picked[index])
            }
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

/// The token just made, with the commands that connect Claude Code to it: in the
/// project it runs in, or in every project of its user.
#[component]
async fn issued_token(issued: &Issued, endpoint: &str) -> Result<impl View> {
    let commands = [
        (
            "Only in the project you run it in",
            claude_code_command(&issued.client, endpoint, &issued.secret, ClaudeScope::Local),
        ),
        (
            "In every project of your user",
            claude_code_command(&issued.client, endpoint, &issued.secret, ClaudeScope::User),
        ),
    ];

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
            <p class="text-sm text-muted-foreground">
                "To connect Claude Code, run one of these in your own terminal, not in a Claude conversation, which would keep the token in its history:"
            </p>
            #[key(index)]
            for (index, (label, command)) in commands.iter().enumerate() {
                <div class="flex flex-col gap-1.5">
                    <p class="text-xs font-medium">(label)</p>
                    <code class="rounded-md border border-border bg-background px-3 py-2 font-mono text-xs break-all whitespace-pre-wrap select-all">
                        (command)
                    </code>
                </div>
            }
        </div>
    })
}

/// One token: its name, when it was made and last used, and a button to revoke it. An
/// organization's token also shows which of the `total` tools turned on it reaches, with
/// a button that opens its tools dialog.
#[component]
async fn token_row(
    token: &McpToken,
    revoking: &Signal<bool>,
    reaching: Option<&Reaching>,
    total: usize,
) -> Result<impl View> {
    let revoking = revoking.clone();
    let label = format!("Revoke {}", token.name);
    let choosing = reaching.map(|reaching| reaching.choosing.clone());

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
                if let Some(reaching) = reaching {
                    reach_summary(reaching: reaching, total: total)
                } else if token.manages() {
                    <span class="flex items-center gap-1 text-xs font-medium text-primary">
                        icon(data: iconify_icon!("lucide:shield-check"), size: 12, attrs: attributes! { class="shrink-0" })
                        "Reads and manages"
                    </span>
                } else {
                    <span class="text-xs text-muted-foreground">"Reads only"</span>
                }
            </div>
            if let Some(choosing) = choosing {
                button(
                    variant: ButtonVariant::Outline,
                    size: ButtonSize::Sm,
                    attrs: attributes! {
                        type="button"
                        aria-label=(format!("Tools of {}", token.name))
                        @click=$(|_e: Event| choosing.set(true))
                    },
                    icon(data: iconify_icon!("lucide:wrench"))
                    "Tools"
                )
            }
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

/// Which of the `total` tools turned on an organization's token reaches, which its tools
/// dialog changes without a reload, with a warning when it reaches none.
#[component]
async fn reach_summary(reaching: &Reaching, total: usize) -> Result<impl View> {
    let all = reaching.all.clone();
    let picked = reaching.picked.clone();
    let every = match total {
        1 => "Reaches the 1 tool turned on".to_owned(),
        total => format!("Reaches all {total} tools turned on"),
    };
    let of_total = match total {
        1 => " of 1 tool turned on".to_owned(),
        total => format!(" of {total} tools turned on"),
    };

    Ok(view! {
        <span class="text-xs text-muted-foreground" :hidden=$(if all.get() { total == 0 } else { true })>(every)</span>
        <span class="text-xs text-muted-foreground" :hidden=$(if all.get() { true } else { picked.get() == 0 })>
            "Reaches " $(picked.get()) (of_total)
        </span>
        <span
            class="flex items-center gap-1 text-xs font-medium text-amber-700 dark:text-amber-400"
            :hidden=$(if all.get() { total > 0 } else { picked.get() > 0 })
        >
            icon(data: iconify_icon!("lucide:triangle-alert"), size: 12, attrs: attributes! { class="shrink-0" })
            "Reaches no tool"
        </span>
    })
}

/// A dialog that picks which of the tools `offered` in the organization's apps `token`
/// reaches, saving each switch as it changes. `picked` are the tools picked for it.
#[component]
async fn tools_dialog(
    cx: &Cx,
    token: &McpToken,
    reaching: &Reaching,
    offered: &[OfferingApp],
    picked: &[(u64, String)],
) -> Result<impl View> {
    let open = reaching.choosing.clone();
    let close = open.clone();
    let all = reaching.all.clone();
    let picked_count = reaching.picked.clone();
    let title_id = element_id(cx);
    let all_id = element_id(cx);
    let token_id = token.id;
    let nothing_offered = offered.is_empty();

    Ok(view! {
        dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Tools of " (&token.name))
                    dialog_description(
                        "Which of the tools turned on in this organization's apps it reaches. Each switch saves at once, and clients already connected see a change once they reconnect."
                    )
                )
                <div class="flex items-start justify-between gap-4 rounded-lg border border-border p-3">
                    <div class="flex flex-col gap-0.5">
                        <label for=(&all_id) class="text-sm font-medium">"Every tool turned on"</label>
                        <p class="text-xs text-muted-foreground">
                            "Also each tool turned on later in an app's MCP tab. Turn it off to pick the ones it reaches."
                        </p>
                    </div>
                    switch(
                        attrs: attributes! {
                            id=(&all_id)
                            class="mt-0.5"
                            :checked=$(all.get())
                            @change=$(async |_e: Event| {
                                let saved = reach_all(token_id, !all.get()).await;
                                all.set(saved);
                            })
                        }
                    )
                </div>
                <div class="flex flex-col gap-3" :hidden=$(all.get())>
                    <p
                        class="flex items-center gap-1.5 text-sm text-amber-700 dark:text-amber-400"
                        :hidden=$(if nothing_offered { true } else { picked_count.get() > 0 })
                    >
                        icon(data: iconify_icon!("lucide:triangle-alert"), size: 14, attrs: attributes! { class="shrink-0" })
                        "It reaches no tool. Pick the ones its client needs."
                    </p>
                    tool_picker(offered: offered, picked: picked, saving: Some((token_id, &reaching.picked)))
                </div>
                dialog_footer(
                    button(
                        attrs: attributes! { type="button" @click=$(|_e: Event| close.set(false)) },
                        "Done"
                    )
                )
            )
        )
    })
}

/// The tools `offered` in the organization's apps, grouped by what they do to their app's
/// data, each with the app it comes from and a switch that picks it, on for those in
/// `picked`.
///
/// With `saving`, a token's ID and its count of tools picked, each switch saves at once
/// and keeps the count. Without it, the switches are fields of the form they are in,
/// named `tool`, as [`issue`] reads them.
#[component]
async fn tool_picker(
    offered: &[OfferingApp],
    picked: &[(u64, String)],
    saving: Option<(u64, &Signal<usize>)>,
) -> Result<impl View> {
    let groups: Vec<_> = EFFECT_GROUPS
        .into_iter()
        .map(|effect| {
            let tools: Vec<_> = offered
                .iter()
                .flat_map(|app| app.tools.iter().map(move |tool| (app, tool)))
                .filter(|(_, tool)| tool.effect == effect)
                .collect();
            (effect, tools)
        })
        .filter(|(_, tools)| !tools.is_empty())
        .collect();

    Ok(view! {
        if offered.is_empty() {
            <p class="text-sm text-muted-foreground">
                "No app of this organization has tools turned on yet. Turn them on in each app's MCP tab."
            </p>
        } else {
            <div class="flex max-h-96 flex-col gap-4 overflow-y-auto">
                #[key(group_key(*effect))]
                for (effect, tools) in &groups {
                    <div class="flex flex-col gap-1.5">
                        effect_heading(effect: *effect, count: tools.len())
                        <ul class="flex flex-col divide-y divide-border rounded-lg border border-border">
                            #[key(format!("{}:{}", tool.service_id, tool.name))]
                            for (app, tool) in tools {
                                tool_choice_row(app: app, tool: tool, on: is_picked(picked, tool), saving: saving)
                            }
                        </ul>
                    </div>
                }
            </div>
        }
    })
}

/// One tool to pick: its name, the app it comes from, what it does, and its switch. See
/// [`tool_picker`].
#[component]
async fn tool_choice_row(
    cx: &Cx,
    app: &OfferingApp,
    tool: &Offered,
    on: bool,
    saving: Option<(u64, &Signal<usize>)>,
) -> Result<impl View> {
    let switch_id = element_id(cx);
    let value = format!("{}:{}", tool.service_id, tool.name);

    Ok(view! {
        <li class="flex items-center gap-3 px-3 py-2">
            <div class="flex min-w-0 flex-1 flex-col gap-0.5">
                <div class="flex flex-wrap items-baseline gap-x-2">
                    <label for=(&switch_id) class="cursor-pointer font-mono text-sm break-all">(&tool.name)</label>
                    <span class="text-xs text-muted-foreground">(&app.label)</span>
                </div>
                if let Some(about) = &tool.about {
                    <p class="line-clamp-1 text-xs text-muted-foreground">(about)</p>
                }
            </div>
            if let Some((token_id, count)) = saving {
                saved_switch(id: &switch_id, token_id: token_id, tool: tool, on: on, count: count)
            } else {
                switch(attrs: attributes! { id=(&switch_id) name="tool" value=(&value) checked=(on.then_some("")) })
            }
        </li>
    })
}

/// The switch that picks `tool` for a token, saving at once and keeping `count`, the
/// number of tools picked for it.
#[component]
async fn saved_switch(
    cx: &Cx,
    id: &str,
    token_id: u64,
    tool: &Offered,
    on: bool,
    count: &Signal<usize>,
) -> Result<impl View> {
    let picked = signal(&cx.keyed(format!("{}:{}", tool.service_id, tool.name)), || on);
    let count = count.clone();
    let service_id = tool.service_id;
    let name = tool.name.clone();

    Ok(view! {
        switch(
            attrs: attributes! {
                id=(id)
                :checked=$(picked.get())
                @change=$(async |_e: Event| {
                    let saved = pick_tool(token_id, service_id, name, !picked.get()).await;
                    picked.set(saved);
                    if saved {
                        count.increment()
                    } else {
                        count.decrement()
                    }
                })
            }
        )
    })
}

/// The groups the lists of tools are in, in order, by what the tools do to their app's
/// data, as each app declares it. `None` holds the tools whose app could not be asked.
pub const EFFECT_GROUPS: [Option<Effect>; 4] = [
    Some(Effect::ReadOnly),
    Some(Effect::Writes),
    Some(Effect::Destructive),
    None,
];

/// What tells a group of [`EFFECT_GROUPS`] apart in a list.
pub fn group_key(effect: Option<Effect>) -> String {
    match effect {
        Some(Effect::ReadOnly) => "read-only",
        Some(Effect::Writes) => "writes",
        Some(Effect::Destructive) => "destructive",
        None => "unknown",
    }
    .to_owned()
}

/// The heading of a group of [`EFFECT_GROUPS`], with how many tools it has. The groups
/// are what each app declares, which oxploy cannot check.
#[component]
pub async fn effect_heading(effect: Option<Effect>, count: usize) -> Result<impl View> {
    let (symbol, title, about, tone) = match effect {
        Some(Effect::ReadOnly) => (
            iconify_icon!("lucide:eye"),
            "Read-only",
            "The app declares they only read its data.",
            "text-foreground",
        ),
        Some(Effect::Writes) => (
            iconify_icon!("lucide:pencil"),
            "Writes",
            "The app declares they change its data without destroying any.",
            "text-foreground",
        ),
        Some(Effect::Destructive) => (
            iconify_icon!("lucide:triangle-alert"),
            "Destructive",
            "The app declares they may delete or overwrite its data, or declares nothing, which counts as that.",
            "text-destructive",
        ),
        None => (
            iconify_icon!("lucide:circle-help"),
            "Not known now",
            "Their app is not running, so oxploy cannot ask what they do.",
            "text-muted-foreground",
        ),
    };

    Ok(view! {
        <div class="flex flex-col gap-0.5">
            <h4 class=(format!("flex items-center gap-1.5 text-sm font-medium {tone}"))>
                icon(data: symbol, size: 14, attrs: attributes! { class="shrink-0" })
                (title)
                <span class="text-xs font-normal text-muted-foreground">(count.to_string())</span>
            </h4>
            <p class="text-xs text-muted-foreground">(about)</p>
        </div>
    })
}

/// A dialog that names a new token of `kind`, for `title`: the server, or the
/// organization's name. An organization's token reaches every tool turned on, or only
/// the ones picked of those `offered`, none at first. A name just turned down comes back
/// in it, with why and what was picked.
#[component]
async fn issue_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    kind: Kind,
    title: &str,
    refused: Option<&Refused>,
    offered: Option<&[OfferingApp]>,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let name_id = element_id(cx);
    let hint_id = element_id(cx);
    let all_id = element_id(cx);
    let picked = refused.and_then(|refused| refused.picked.as_deref());
    let all = signal(cx, || refused.is_some_and(|refused| refused.picked.is_none()));
    let manages_id = element_id(cx);
    let manages = refused.is_some_and(|refused| refused.manages);
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
            "A token for one client, such as Claude Code. It reads the server and every app on it, and is shown only once.",
        ),
        Kind::Organization => (
            format!("New token for {title}"),
            "A token for one client, such as a bot. It reaches the tools you pick of those turned on in this organization's apps, and is shown only once.",
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
                    if kind == Kind::Server {
                        <div class="flex items-start justify-between gap-4 rounded-lg border border-border p-3">
                            <div class="flex flex-col gap-0.5">
                                <label for=(&manages_id) class="text-sm font-medium">"Can manage"</label>
                                <p class="text-xs text-muted-foreground">
                                    "It also installs oxploy's updates and manages the organizations' tokens and the tools turned on in their apps, over MCP. Clients such as Claude Code ask you before each change, unless told not to. Server tokens are only made here."
                                </p>
                            </div>
                            switch(attrs: attributes! { id=(&manages_id) name="manages" class="mt-0.5" checked=(manages.then_some("")) })
                        </div>
                    }
                    if let Some(offered) = offered {
                        <div class="flex flex-col gap-3">
                            <div class="flex items-start justify-between gap-4 rounded-lg border border-border p-3">
                                <div class="flex flex-col gap-0.5">
                                    <label for=(&all_id) class="text-sm font-medium">"Every tool turned on"</label>
                                    <p class="text-xs text-muted-foreground">
                                        "Also each tool turned on later in an app's MCP tab. Leave it off to pick the ones it reaches."
                                    </p>
                                </div>
                                switch(
                                    attrs: attributes! {
                                        id=(&all_id)
                                        name="all"
                                        class="mt-0.5"
                                        :checked=$(all.get())
                                        @change=$(|_e: Event| all.set(!all.get()))
                                    }
                                )
                            </div>
                            <div class="flex flex-col gap-2" :hidden=$(all.get())>
                                <p class="text-sm font-medium">"Tools it reaches"</p>
                                tool_picker(offered: offered, picked: picked.unwrap_or_default(), saving: None)
                            </div>
                        </div>
                    }
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
