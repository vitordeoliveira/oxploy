use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    runtime::signal,
    view::{Attributes, View, attributes, class, component, view},
};

use super::input::input;

/// An [`input`] for a password, with a button that shows it as plain text.
///
/// Pass input attributes through `attrs`, as with [`input`]. The type is managed by the
/// component, so leave it out.
///
/// ```ignore
/// view! {
///     password_input(attrs: attributes! { name="password" autocomplete="current-password" })
/// }
/// ```
#[component]
pub async fn password_input(cx: &Cx, #[default] mut attrs: Attributes) -> Result<impl View> {
    // Whether the password is shown as plain text, kept in the browser.
    let shown = signal(cx, || false);

    Ok(view! {
        <div class="relative w-full">
            input(
                attrs: attributes! {
                    :type=$(if shown.get() { "text" } else { "password" })
                    class=(class!("pr-10", attrs.remove("class")))
                    (attrs)
                }
            )
            <button
                type="button"
                class="absolute inset-y-0 right-0 flex items-center pr-3 text-muted-foreground outline-none hover:text-foreground focus-visible:text-foreground"
                :aria-label=$(if shown.get() { "Hide password" } else { "Show password" })
                @click=$(|_e| shown.toggle())
            >
                icon(
                    data: iconify_icon!("lucide:eye"),
                    size: 16,
                    attrs: attributes! { :hidden=$(shown.get()) }
                )
                icon(
                    data: iconify_icon!("lucide:eye-off"),
                    size: 16,
                    attrs: attributes! { :hidden=$(!shown.get()) }
                )
            </button>
        </div>
    })
}
