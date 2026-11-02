use topcoat::{
    Result,
    context::Cx,
    icon::{IconData, icon, iconify::iconify_icon},
    router::{href, page},
    runtime::{Event, signal},
    view::{View, attributes, component, element_id, view},
};

use crate::components::button::{ButtonSize, button};
use crate::components::input::input;
use crate::components::label::label;

/// The oxploy logo, a cog like the one in the Rust logo.
const LOGO: IconData = iconify_icon!("lucide:cog");

#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    // Whether the password is shown as plain text, kept in the browser.
    let show_password = signal(cx, || false);
    let email = element_id(cx);
    let password = element_id(cx);

    Ok(view! {
        <div class="grid min-h-svh lg:grid-cols-2">
            <div class="flex flex-col gap-4 p-6 md:p-10">
                <a class="flex items-center gap-2 self-start font-semibold" href=(href!(super::home))>
                    <span class="flex size-8 items-center justify-center rounded-lg bg-primary text-primary-foreground">
                        icon(data: LOGO, size: 20)
                    </span>
                    "oxploy"
                </a>
                <div class="flex flex-1 items-center justify-center">
                    <div class="flex w-full max-w-sm flex-col gap-6">
                        <div class="flex flex-col gap-2">
                            <h1 class="text-2xl font-semibold tracking-tight">"Welcome back"</h1>
                            <p class="text-sm text-muted-foreground">
                                "Sign in to manage your apps and deployments."
                            </p>
                        </div>
                        // Sign-in is not wired yet. The POST keeps the credentials out of the
                        // URL if the form is submitted before the runtime loads.
                        <form
                            class="flex flex-col gap-4"
                            method="post"
                            @submit=$(|e: Event| e.prevent_default())
                        >
                            <div class="flex flex-col gap-2">
                                label(attrs: attributes! { for=(&email) }, "Email")
                                input(
                                    attrs: attributes! {
                                        id=(&email)
                                        type="email"
                                        name="email"
                                        placeholder="you@example.com"
                                        autocomplete="email"
                                        required=""
                                    }
                                )
                            </div>
                            <div class="flex flex-col gap-2">
                                label(attrs: attributes! { for=(&password) }, "Password")
                                <div class="relative w-full">
                                    input(
                                        attrs: attributes! {
                                            id=(&password)
                                            :type=$(if show_password.get() { "text" } else { "password" })
                                            name="password"
                                            placeholder="••••••••"
                                            autocomplete="current-password"
                                            required=""
                                            class="pr-10"
                                        }
                                    )
                                    <button
                                        type="button"
                                        class="absolute inset-y-0 right-0 flex items-center pr-3 text-muted-foreground outline-none hover:text-foreground focus-visible:text-foreground"
                                        :aria-label=$(if show_password.get() { "Hide password" } else { "Show password" })
                                        @click=$(|_e| show_password.toggle())
                                    >
                                        icon(
                                            data: iconify_icon!("lucide:eye"),
                                            size: 16,
                                            attrs: attributes! { :hidden=$(show_password.get()) }
                                        )
                                        icon(
                                            data: iconify_icon!("lucide:eye-off"),
                                            size: 16,
                                            attrs: attributes! { :hidden=$(!show_password.get()) }
                                        )
                                    </button>
                                </div>
                            </div>
                            button(
                                size: ButtonSize::Lg,
                                attrs: attributes! { type="submit" class="mt-2 w-full" },
                                "Sign in"
                                icon(data: iconify_icon!("lucide:arrow-right"), size: 16)
                            )
                        </form>
                    </div>
                </div>
                <p class="text-xs text-muted-foreground">"oxploy v" (env!("CARGO_PKG_VERSION"))</p>
            </div>
            <div class="relative hidden overflow-hidden border-l border-border bg-muted lg:flex lg:flex-col lg:justify-center lg:p-12 xl:p-16">
                // A faint grid that fades out from the top right corner, and a rust glow
                // in the opposite corner.
                <div class="absolute inset-0 bg-[linear-gradient(to_right,var(--border)_1px,transparent_1px),linear-gradient(to_bottom,var(--border)_1px,transparent_1px)] bg-size-[48px_48px] mask-radial-at-top-right mask-radial-from-0% mask-radial-to-80%"></div>
                <div class="absolute -bottom-40 -left-40 size-[32rem] rounded-full bg-primary/15 blur-3xl"></div>
                <div class="relative flex max-w-lg flex-col gap-10">
                    <div class="flex flex-col gap-3">
                        <h2 class="text-3xl font-semibold tracking-tight text-balance">
                            "Push a Rust repo. "
                            <span class="text-primary">"Get a running binary."</span>
                        </h2>
                        <p class="text-muted-foreground">
                            "Release builds with cached dependencies, one isolated container per app, and logs as they happen."
                        </p>
                    </div>
                    build_log()
                </div>
            </div>
        </div>
    })
}

/// An illustrative build log, styled after cargo's output.
#[component]
async fn build_log() -> Result<impl View> {
    Ok(view! {
        <div class="overflow-hidden rounded-xl border border-border bg-background/80 shadow-sm backdrop-blur">
            <div class="flex items-center justify-between border-b border-border px-4 py-2.5 text-xs">
                <span class="flex items-center gap-2 text-muted-foreground">
                    icon(data: iconify_icon!("lucide:square-terminal"), size: 14)
                    "my-api · deploy #42"
                </span>
                <span class="flex items-center gap-1.5 font-medium text-primary">
                    <span class="size-1.5 rounded-full bg-primary"></span>
                    "live"
                </span>
            </div>
            <div class="p-4 font-mono text-xs leading-6 whitespace-pre">
                <p><span class="text-primary">"$ "</span>"git pull origin main"</p>
                <p><span class="text-primary">"$ "</span>"cargo build --release"</p>
                <p class="text-muted-foreground">
                    <span class="font-semibold text-primary">"   Compiling"</span>
                    " my-api v0.4.2 (/app)"
                </p>
                <p class="text-muted-foreground">
                    <span class="font-semibold text-primary">"    Finished"</span>
                    " `release` profile in 38.12s"
                </p>
                <p class="text-muted-foreground">"▸ starting my-api 4f2c1a9"</p>
                <p>"✓ listening on 0.0.0.0:8080"</p>
            </div>
        </div>
    })
}
