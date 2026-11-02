use topcoat::{
    Result,
    icon::{icon, iconify::iconify_icon},
    router::page,
    view::{View, view},
};

/// The projects of the active organization. Projects do not exist yet, so the page
/// shows its heading and an empty state.
#[page]
pub async fn page() -> Result<impl View> {
    Ok(view! {
        <div class="flex flex-1 flex-col gap-6">
            <div class="flex flex-col gap-1">
                <h1 class="text-2xl font-semibold tracking-tight">"Projects"</h1>
                <p class="text-sm text-muted-foreground">
                    "Group the apps and databases you deploy."
                </p>
            </div>
            <div class="flex flex-1 flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border p-10 text-center">
                <span class="flex size-12 items-center justify-center rounded-xl bg-primary/10 text-primary">
                    icon(data: iconify_icon!("lucide:layers"), size: 24)
                </span>
                <h2 class="font-medium">"No projects yet"</h2>
                <p class="max-w-sm text-sm text-muted-foreground">
                    "Projects created in this organization will show up here."
                </p>
            </div>
        </div>
    })
}
