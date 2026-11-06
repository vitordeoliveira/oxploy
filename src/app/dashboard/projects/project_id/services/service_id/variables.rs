//! The Environment tab: the service's variables, written like a `.env` file, and every
//! variable the app gets with where it comes from.

use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    runtime::{Event, signal},
    view::{View, attributes, component, element_id, view},
};

use super::settings_card;
use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::field::{field, field_description, field_error, field_label};
use crate::components::textarea::textarea;
use crate::features::app_files::AppFiles;
use crate::features::databases::{self, ManagedDatabase};
use crate::features::environment::{
    self, MAX_LEN, MCP_TOKEN_VARIABLE, PORT_VARIABLE, STORAGE_VARIABLE,
};
use crate::features::runtime::{system_env, variables_with_port};
use crate::features::service::Service;

/// Variables the user submitted that could not be saved, with what is wrong.
pub struct Draft {
    pub text: String,
    pub problem: String,
}

/// The service's variables in an editor that posts them to `here`. Values stay blurred
/// until the editor has focus or the user shows them, so they do not leak on a shared
/// screen.
#[component]
pub async fn environment_tab(
    cx: &Cx,
    service: &Service,
    here: &str,
    draft: Option<&Draft>,
) -> Result<impl View> {
    let saved = service.environment.as_deref().unwrap_or("");
    let managed = ManagedDatabase::of_service(cx, service.id).await?;
    let count = environment::parse(saved, &databases::set_elsewhere(&managed))
        .map_or(0, |variables| variables.len());
    let count = match count {
        1 => "1 variable".to_owned(),
        count => format!("{count} variables"),
    };
    // A draft comes back to be fixed, so its values show.
    let revealed = signal(cx, || draft.is_some());
    let reveal = revealed.clone();
    let id = element_id(cx);
    let hint_id = element_id(cx);

    Ok(view! {
        <div class="flex flex-col gap-6">
        settings_card(
            title: "Environment variables",
            text: "Passed to cargo while it builds and to the binary when it runs. The app reads the ones it needs.",
            <form method="post" action=(here) class="flex flex-col gap-4">
                field(
                    <div class="flex items-center justify-between gap-2">
                        field_label(attrs: attributes! { for=(&id) }, "Variables")
                        button(
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::Sm,
                            attrs: attributes! {
                                type="button"
                                @click=$(|_e: Event| reveal.toggle())
                            },
                            icon(data: iconify_icon!("lucide:eye"))
                            $(if revealed.get() { "Hide values" } else { "Show values" })
                        )
                    </div>
                    <div
                        :class=$(if revealed.get() {
                            "contents"
                        } else {
                            "contents [&_textarea:not(:focus)]:blur-[3px]"
                        })
                    >
                        textarea(
                            attrs: attributes! {
                                id=(&id)
                                name="variables"
                                class="min-h-64 font-mono leading-relaxed"
                                placeholder="DATABASE_URL=postgres://user:password@db:5432/shop\nRUST_LOG=info"
                                maxlength=(MAX_LEN.to_string())
                                spellcheck="false"
                                autocomplete="off"
                                autocapitalize="off"
                                wrap="off"
                                aria-describedby=(&hint_id)
                                aria-invalid=(draft.is_some().then_some("true"))
                            },
                            (draft.map_or(saved, |draft| draft.text.as_str()))
                        )
                    </div>
                    if let Some(draft) = draft {
                        field_error(attrs: attributes! { id=(&hint_id) }, (&draft.problem))
                    } else {
                        field_description(
                            // The example is one long word, which would widen a phone's page.
                            attrs: attributes! { id=(&hint_id) class="[overflow-wrap:anywhere]" },
                            "One KEY=value per line, like a .env file; lines starting with # are comments. Values can use what oxploy and the Storage tab set, listed below, or a variable from an earlier line, as in DATABASE_URL=sqlite:${OXPLOY_STORAGE_DIR}/data.db."
                        )
                    }
                )
                <div class="flex items-center justify-between gap-4">
                    <span class="text-sm text-muted-foreground">(count) " saved"</span>
                    button(attrs: attributes! { type="submit" }, "Save variables")
                </div>
            </form>
        )
        available_card(service: service, managed: &managed)
        </div>
    })
}

/// One variable the app gets.
struct Row {
    name: String,
    /// Its value, or `None` while it has none yet.
    value: Option<String>,
    /// Where it points, or why it has no value.
    note: Option<String>,
    /// Whether the value may be a secret, which stays blurred until shown.
    secret: bool,
}

/// Variables that come from one place.
struct Group {
    title: &'static str,
    text: &'static str,
    rows: Vec<Row>,
}

/// Every variable the app gets when it runs, grouped by where it comes from, with the
/// values it gets: `${NAME}` filled in, as from the saved variables.
#[component]
async fn available_card(cx: &Cx, service: &Service, managed: &[ManagedDatabase]) -> Result<impl View> {
    let files = AppFiles::of(&service.app_name);
    let port = service
        .port
        .map_or_else(|| format!("${{{PORT_VARIABLE}}}"), |port| port.to_string());
    let resolved = variables_with_port(service, &files, &port, managed);
    let databases: Vec<(&str, &str)> = managed
        .iter()
        .filter_map(|database| Some((database.variable.as_deref()?, database.file.as_str())))
        .collect();
    let row = |name: &str, value: &str, note: Option<String>, secret: bool| Row {
        name: name.to_owned(),
        value: Some(value.to_owned()),
        note,
        secret,
    };

    let yours: Vec<Row> = resolved
        .iter()
        .filter(|(name, _)| name != MCP_TOKEN_VARIABLE && !databases.iter().any(|(set, _)| set == name))
        .map(|(name, value)| row(name, value, None, true))
        .collect();
    let mut by_oxploy = vec![
        Row {
            name: PORT_VARIABLE.to_owned(),
            value: service.port.map(|port| port.to_string()),
            note: Some(match service.port {
                Some(_) => "The port the app listens on at 127.0.0.1, which Caddy sends its traffic to.".to_owned(),
                None => "Chosen at the first deploy.".to_owned(),
            }),
            secret: false,
        },
        row(
            STORAGE_VARIABLE,
            &files.storage().display().to_string(),
            Some("The folder for the app's data, which deploys and restarts keep.".to_owned()),
            false,
        ),
    ];
    if let Some(token) = service.mcp_token_in_use() {
        by_oxploy.push(row(
            MCP_TOKEN_VARIABLE,
            token,
            Some("The token oxploy calls the app's MCP server with, from the MCP tab.".to_owned()),
            true,
        ));
    }
    let from_storage: Vec<Row> = resolved
        .iter()
        .filter_map(|(name, value)| {
            let (_, file) = databases.iter().find(|(set, _)| set == name)?;
            Some(row(name, value, Some(format!("The database {file}, in the Storage tab.")), false))
        })
        .collect();
    let from_server: Vec<Row> = system_env()
        .map(|(name, value)| row(name, &value, None, false))
        .collect();

    let mut groups = vec![
        Group {
            title: "Yours",
            text: "The variables above, with ${NAME} filled in.",
            rows: yours,
        },
        Group {
            title: "Set by oxploy",
            text: "",
            rows: by_oxploy,
        },
    ];
    if !from_storage.is_empty() {
        groups.push(Group {
            title: "From the Storage tab",
            text: "",
            rows: from_storage,
        });
    }
    groups.push(Group {
        title: "From the server",
        text: "What programs need, from the environment oxploy runs in.",
        rows: from_server,
    });
    let revealed = signal(cx, || false);
    let reveal = revealed.clone();

    Ok(view! {
        settings_card(
            title: "What the app gets",
            text: "Every variable the app gets when it runs, and where each comes from.",
            <div class="flex items-center justify-between gap-2">
                <p class="text-sm text-muted-foreground">"Values that may be secret stay blurred."</p>
                button(
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Sm,
                    attrs: attributes! { type="button" @click=$(|_e: Event| reveal.toggle()) },
                    icon(data: iconify_icon!("lucide:eye"))
                    $(if revealed.get() { "Hide values" } else { "Show values" })
                )
            </div>
            <div
                :class=$(if revealed.get() {
                    "flex flex-col gap-5"
                } else {
                    "flex flex-col gap-5 [&_[data-secret]]:blur-[3px]"
                })
            >
                #[key(group.title)]
                for group in &groups {
                    <div class="flex flex-col gap-2">
                        <div class="flex flex-col">
                            <h3 class="text-sm font-medium">(group.title)</h3>
                            if !group.text.is_empty() {
                                <p class="text-xs text-muted-foreground">(group.text)</p>
                            }
                        </div>
                        if group.rows.is_empty() {
                            <p class="text-sm text-muted-foreground">"None yet."</p>
                        } else {
                            <ul class="divide-y divide-border rounded-lg border border-border">
                                #[key(row.name.clone())]
                                for row in &group.rows {
                                    <li class="grid grid-cols-1 gap-x-4 gap-y-0.5 px-4 py-2.5 text-sm sm:grid-cols-[minmax(0,14rem)_minmax(0,1fr)]">
                                        <code class="font-mono font-medium break-all">(&row.name)</code>
                                        <div class="flex min-w-0 flex-col gap-0.5">
                                            match row.value.as_deref() {
                                                Some("") => {
                                                    <span class="text-muted-foreground italic">"Empty"</span>
                                                }
                                                Some(value) => {
                                                    <code
                                                        class="font-mono break-all text-muted-foreground"
                                                        data-secret=(row.secret.then_some(""))
                                                    >
                                                        (value)
                                                    </code>
                                                }
                                                None => {}
                                            }
                                            if let Some(note) = &row.note {
                                                <span class="text-xs text-muted-foreground">(note)</span>
                                            }
                                        </div>
                                    </li>
                                }
                            </ul>
                        }
                    </div>
                }
            </div>
            field_description(
                "Builds get the same, except PORT and OXPLOY_STORAGE_DIR, which reach only the app and the script after the build. Builds also get CARGO_TARGET_DIR, the app's cargo cache, and a build script gets OXPLOY_OUTPUT_DIR, where it leaves the binary."
            )
        )
    })
}
