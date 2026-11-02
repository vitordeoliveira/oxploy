fn main() {
    // Scan the package for Tailwind classes and generate the stylesheet.
    topcoat::tailwind::BuildConfig::new()
        .input("styles.css")
        .render()
        .unwrap();

    // Download the Iconify sets used by `iconify_icon!` and stage them for compilation.
    topcoat::icon::iconify::BuildConfig::new()
        .icon_set("lucide")
        .stage()
        .unwrap();
}
