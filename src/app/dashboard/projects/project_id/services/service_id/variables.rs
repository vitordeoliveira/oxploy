//! The Environment tab: the service's variables, written like a `.env` file.

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
use crate::features::environment::{self, MAX_LEN};
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
    let count = environment::parse(saved).map_or(0, |variables| variables.len());
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
                            attrs: attributes! { id=(&hint_id) },
                            "One KEY=value per line, like a .env file; lines starting with # are comments. Values can use ${PORT}, ${OXPLOY_STORAGE_DIR} or a variable from an earlier line, as in DATABASE_URL=sqlite:${OXPLOY_STORAGE_DIR}/data.db."
                        )
                    }
                )
                <div class="flex items-center justify-between gap-4">
                    <span class="text-sm text-muted-foreground">(count) " saved"</span>
                    button(attrs: attributes! { type="submit" }, "Save variables")
                </div>
            </form>
        )
    })
}
