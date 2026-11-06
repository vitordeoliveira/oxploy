//! The MCP page: where AI clients, such as a bot or Claude Code, read this server and its
//! apps over the Model Context Protocol, at `/mcp`.
//!
//! The super admin makes server tokens here, which read the machine, oxploy itself and
//! every app it runs. Each organization will get tokens of its own, for the tools of its
//! apps.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{SeeOther, bad_request, see_other},
        href, page, route,
    },
    runtime::{Event, Signal, signal},
    view::{View, attributes, component, element_id, view},
};

use super::projects::created_at;
use super::{panel_url, require_server_owner, require_sign_in};
use crate::components::alert_dialog::alert_dialog;
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::dialog::{
    dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title,
};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::features::mcp::{MAX_NAME_LEN, McpToken};
use crate::features::user::Role;

/// A token just made, which the page shows this once.
struct Issued {
    name: String,
    secret: String,
}

/// The MCP page.
#[page]
pub async fn page() -> Result<impl View> {
    Ok(view! { mcp_view() })
}

/// The form that makes a token.
#[derive(Deserialize)]
struct IssueForm {
    name: String,
}

/// Makes a server token, and shows the page with it: the only time it is shown, since
/// only its hash is kept.
#[page(POST)]
async fn issue(cx: &Cx, Form(form): Form<IssueForm>) -> Result<impl View> {
    require_server_owner(cx).await?;
    let Some((token, secret)) = McpToken::issue(cx, &form.name).await? else {
        return Err(bad_request(format!("A token needs a name of 1 to {MAX_NAME_LEN} characters")).into());
    };
    let issued = Issued {
        name: token.name,
        secret,
    };

    Ok(view! { mcp_view(issued: Some(&issued)) })
}

/// The form that revokes a token.
#[derive(Deserialize)]
struct RevokeForm {
    id: u64,
}

/// Revokes a server token: clients that send it are turned away from then on.
#[route(POST "./revoke")]
async fn revoke(cx: &Cx, Form(form): Form<RevokeForm>) -> Result<SeeOther> {
    require_server_owner(cx).await?;
    McpToken::revoke(cx, form.id).await?;
    Ok(see_other(href!(page).resolve(cx)))
}

/// The page, with the token just made in `issued`.
#[component]
async fn mcp_view(cx: &Cx, #[default] issued: Option<&Issued>) -> Result<impl View> {
    let signed_in = require_sign_in(cx).await?;
    let owns_server = signed_in.user.role == Role::SuperAdmin;

    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            <div class="flex flex-col gap-1">
                <h1 class="text-2xl font-semibold tracking-tight">"MCP"</h1>
                <p class="text-sm text-muted-foreground">
                    "Where AI clients, such as a bot or Claude Code, read this server and its apps."
                </p>
            </div>
            if owns_server {
                server_section(issued: issued)
            }
            <section class="flex flex-wrap items-center gap-3 rounded-xl border border-border bg-card p-5">
                <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-foreground/5">
                    icon(data: iconify_icon!("lucide:boxes"), size: 18)
                </span>
                <div class="flex min-w-56 flex-1 flex-col">
                    <h2 class="font-semibold">"This organization"</h2>
                    <p class="text-sm text-muted-foreground">
                        "Tokens of its own, which reach the tools of this organization's apps that you allow. Other organizations have their own."
                    </p>
                </div>
                <span class="rounded-full border border-border px-2.5 py-0.5 text-xs text-muted-foreground">
                    "Coming soon"
                </span>
            </section>
        </div>
    })
}

/// The server's MCP: its address, the token just made, and the tokens to revoke.
#[component]
async fn server_section(cx: &Cx, issued: Option<&Issued>) -> Result<impl View> {
    let tokens = McpToken::all_newest_first(cx).await?;
    let endpoint = format!("{}/mcp", panel_url(cx).unwrap_or_default());
    let issuing = signal(cx, || false);
    let open_issuing = issuing.clone();
    let revoking: Vec<_> = tokens
        .iter()
        .map(|token| signal(&cx.keyed(token.id), || false))
        .collect();

    Ok(view! {
        <section class="flex flex-col rounded-xl border border-border bg-card">
            <header class="flex flex-wrap items-center gap-3 border-b border-border p-5">
                <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-foreground/5">
                    icon(data: iconify_icon!("lucide:server"), size: 18)
                </span>
                <div class="flex min-w-56 flex-1 flex-col">
                    <h2 class="font-semibold">"Server"</h2>
                    <p class="text-sm text-muted-foreground">
                        "Reads the machine, oxploy itself, and the usage, logs and deployments of every app on it. Only you see this, as the server's owner, and it is the same in every organization."
                    </p>
                </div>
                button(
                    attrs: attributes! { type="button" @click=$(|_e: Event| open_issuing.set(true)) },
                    icon(data: iconify_icon!("lucide:plus"))
                    "New token"
                )
            </header>
            if let Some(issued) = issued {
                issued_token(issued: issued, endpoint: &endpoint)
            }
            <div class="flex flex-col gap-2 border-b border-border p-5">
                <span class="text-sm font-medium">"Endpoint"</span>
                <code class="w-fit max-w-full rounded-md bg-muted px-2 py-1 font-mono text-sm break-all select-all">
                    (&endpoint)
                </code>
                <p class="text-sm text-muted-foreground">
                    "Clients send a token as "
                    <code class="font-mono text-foreground">"Authorization: Bearer <token>"</code>
                    ". The tools only read: changes are made here, in the panel."
                </p>
            </div>
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
        issue_dialog(open: &issuing)
        #[key(token.id)]
        for (index, token) in tokens.iter().enumerate() {
            revoke_dialog(open: &revoking[index], token: token)
        }
    })
}

/// The token just made, with the command that connects Claude Code to it.
#[component]
async fn issued_token(issued: &Issued, endpoint: &str) -> Result<impl View> {
    let command = format!(
        "claude mcp add --transport http oxploy {endpoint} --header \"Authorization: Bearer {}\"",
        issued.secret
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

/// A dialog that names a new server token.
#[component]
async fn issue_dialog(cx: &Cx, open: &Signal<bool>) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let name_id = element_id(cx);

    Ok(view! {
        dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "New server token")
                    dialog_description(
                        "A token for one client, such as a bot. It reads the server and every app on it, and is shown only once."
                    )
                )
                <form method="post" action=(href!(page)) class="flex flex-col gap-4">
                    field(
                        field_label(attrs: attributes! { for=(&name_id) }, "Name")
                        input(
                            attrs: attributes! {
                                id=(&name_id)
                                name="name"
                                placeholder="Telegram bot"
                                maxlength=(MAX_NAME_LEN.to_string())
                                required=""
                                autocomplete="off"
                            }
                        )
                        field_description("What uses it, so you know which one to revoke.")
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
async fn revoke_dialog(cx: &Cx, open: &Signal<bool>, token: &McpToken) -> Result<impl View> {
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
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Revoke token")
                    dialog_description(
                        attrs: attributes! { id=(&description_id) },
                        "Clients that use "
                        <strong class="font-medium text-foreground">(&token.name)</strong>
                        " will no longer reach the server. A revoked token cannot be brought back."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(href!(revoke))>
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
