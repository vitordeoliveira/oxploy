use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    runtime::{Event, signal},
    view::{View, attributes, component, view},
};

/// `text` in a button that copies exactly it, and nothing around it, to the clipboard.
///
/// It shows a check once copied, until the pointer or focus leaves it. Inline by
/// default, for a variable or a short value in a sentence; `block` makes it a full-width
/// box, for a secret or a command.
///
/// ```ignore
/// view! {
///     copy_text(text: "${OXPLOY_MCP_RECEPTIONIST}")
///     copy_text(text: &command, block: true)
/// }
/// ```
#[component]
pub async fn copy_text(cx: &Cx, text: &str, #[default] block: bool) -> Result<impl View> {
    let text = text.to_owned();
    // Keyed by the text, so two of them side by side do not share it.
    let copied = signal(&cx.keyed(&text), || false);
    let shown = text.clone();

    Ok(view! {
        <button
            type="button"
            class=(if block {
                "group flex w-full items-start justify-between gap-3 rounded-md border border-border bg-background px-3 py-2 text-left font-mono text-xs break-all whitespace-pre-wrap outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring cursor-pointer"
            } else {
                "group inline-flex max-w-full items-center gap-1.5 rounded border border-border bg-muted/50 px-1.5 py-0.5 text-left font-mono text-xs text-foreground break-all outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring cursor-pointer"
            })
            :title=$(if copied.get() { "Copied" } else { "Click to copy" })
            :aria-label=$(if copied.get() { "Copied" } else { "Copy" })
            @click=$(|_e: Event| {
                copied.set(true);
                // The Clipboard API needs HTTPS or localhost, so a panel on plain HTTP
                // copies through a hidden textarea instead. It comes last and starts with
                // `void`, so the generated script does not run it into the next line.
                raw!(
                    r"void ((text) => {
                        if (window.isSecureContext && navigator.clipboard) {
                            navigator.clipboard.writeText(text).catch(() => {});
                            return;
                        }
                        const area = document.createElement('textarea');
                        area.value = text;
                        area.setAttribute('readonly', '');
                        area.style.position = 'fixed';
                        area.style.opacity = '0';
                        document.body.append(area);
                        area.select();
                        document.execCommand('copy');
                        area.remove();
                    })(${text}.toString())",
                    ()
                );
            })
            @mouseleave=$(|_e: Event| copied.set(false))
            @blur=$(|_e: Event| copied.set(false))
        >
            <span class="min-w-0">(shown)</span>
            <span class=(if block {
                "inline-flex shrink-0 pt-0.5 text-muted-foreground group-hover:text-foreground"
            } else {
                "inline-flex shrink-0 text-muted-foreground group-hover:text-foreground"
            })>
                icon(data: iconify_icon!("lucide:copy"), size: 12, attrs: attributes! { :hidden=$(copied.get()) })
                icon(data: iconify_icon!("lucide:check"), size: 12, attrs: attributes! { class="text-emerald-600 dark:text-emerald-400" :hidden=$(!copied.get()) })
            </span>
        </button>
    })
}
