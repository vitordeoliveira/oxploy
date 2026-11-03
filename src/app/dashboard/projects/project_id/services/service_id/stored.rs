//! The Storage tab: the service's storage folder, its SQLite databases, their backups,
//! and its other files.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    icon::{icon, iconify::iconify_icon},
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        header::{CONTENT_DISPOSITION, CONTENT_TYPE, HeaderName},
        href, query_params, route,
    },
    runtime::{Event, Signal, signal},
    view::{View, attributes, component, element_id, view},
};

use super::super::super::ProjectId;
use super::{ServiceId, ServiceTab, current_service, settings_card};
use crate::app::dashboard::monitoring::format_bytes;
use crate::app::dashboard::projects::created_at;
use crate::components::alert_dialog::alert_dialog;
use crate::components::button::{ButtonSize, ButtonVariant, button, button_variants};
use crate::components::dialog::{
    dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title,
};
use crate::components::field::{field, field_description, field_label};
use crate::components::input::input;
use crate::components::select::select;
use crate::features::app_files::{AppFiles, size_of};
use crate::features::databases::{ManagedDatabase, NewDatabase, ValueFormat};
use crate::features::deploy::Deployer;
use crate::features::environment::{self, STORAGE_VARIABLE};
use crate::features::project::Project;
use crate::features::service::Service;
use crate::features::storage::{self, Backup, Database, StoredFile};

/// The service's storage folder and what is in it.
#[component]
pub async fn storage_tab(cx: &Cx, project: &Project, service: &Service) -> Result<impl View> {
    let files = AppFiles::of(&service.app_name);
    let folder = files.storage();
    let read = {
        let files = files.clone();
        move || {
            (
                storage::databases(&files),
                storage::backups(&files),
                storage::other_files(&files),
                size_of(&files.storage()),
            )
        }
    };
    let (databases, backups, others, used) = tokio::task::spawn_blocking(read).await?;
    let managed = ManagedDatabase::of_service(cx, service.id).await?;
    // Each database once: the ones oxploy manages first, then the others in the folder.
    let mut entries: Vec<Entry> = managed
        .iter()
        .enumerate()
        .map(|(index, database)| Entry {
            file: database.file.clone(),
            managed: Some(index),
            found: databases.iter().position(|found| found.path == database.file),
        })
        .collect();
    for (index, found) in databases.iter().enumerate() {
        if !managed.iter().any(|database| database.file == found.path) {
            entries.push(Entry {
                file: found.path.clone(),
                managed: None,
                found: Some(index),
            });
        }
    }
    // `DATABASE_URL` is suggested unless something already sets it.
    let own = environment::parse(service.environment.as_deref().unwrap_or(""), &[])
        .unwrap_or_default();
    let taken = |name: &str| {
        own.iter().any(|variable| variable.key == name)
            || managed
                .iter()
                .any(|database| database.variable.as_deref() == Some(name))
    };
    let suggested_variable = if taken("DATABASE_URL") { "" } else { "DATABASE_URL" };
    let advanced_url = ServiceTab::Advanced.url(cx, project, service);
    let adding = signal(cx, || false);
    let open_adding = adding.clone();
    // Keyed by file, so each database gets dialogs of its own: managing one found in the
    // folder, or disconnecting and deleting a managed one.
    let entry_dialogs: Vec<_> = entries
        .iter()
        .map(|entry| {
            let cx = cx.keyed(entry.file.clone());
            (signal(&cx, || false), signal(&cx, || false))
        })
        .collect();
    let (project_id, service_id) = (ProjectId(project.id), ServiceId(service.id));
    let urls = Urls {
        back_up: href!(back_up, project_id, service_id).resolve(cx),
        restore: href!(restore, ProjectId(project.id), ServiceId(service.id)).resolve(cx),
        delete: href!(delete_backup, ProjectId(project.id), ServiceId(service.id)).resolve(cx),
        download: href!(download, ProjectId(project.id), ServiceId(service.id)).resolve(cx),
        add: href!(add_database, ProjectId(project.id), ServiceId(service.id)).resolve(cx),
        disconnect: href!(disconnect_database, ProjectId(project.id), ServiceId(service.id))
            .resolve(cx),
        delete_database: href!(delete_database, ProjectId(project.id), ServiceId(service.id))
            .resolve(cx),
    };
    // Keyed by backup, so each one gets dialogs of its own.
    let dialogs: Vec<_> = backups
        .iter()
        .map(|backup| {
            let cx = cx.keyed((backup.database.clone(), backup.name.clone()));
            (signal(&cx, || false), signal(&cx, || false))
        })
        .collect();

    Ok(view! {
        <div class="flex flex-col gap-6">
            settings_card(
                title: "Storage",
                text: "A folder for the app's data. Deploys and restarts keep it, and deleting the service deletes it.",
                <dl class="grid grid-cols-1 gap-x-6 gap-y-3 text-sm sm:grid-cols-[8rem_minmax(0,1fr)]">
                    <dt class="font-medium">"Folder"</dt>
                    <dd class="font-mono break-all text-muted-foreground">(folder.display().to_string())</dd>
                    <dt class="font-medium">"Variable"</dt>
                    <dd class="font-mono text-muted-foreground">(STORAGE_VARIABLE)</dd>
                    <dt class="font-medium">"Used"</dt>
                    <dd class="text-muted-foreground">(if used == 0 { "Nothing yet".to_owned() } else { format_bytes(used) })</dd>
                </dl>
            )
            settings_card(
                title: "Databases",
                text: "SQLite databases in the storage folder. oxploy connects the ones it manages to the app, through a variable, a link in each release, or both.",
                <div class="flex flex-wrap items-start justify-between gap-3">
                    <p class="flex max-w-2xl items-start gap-2 text-sm text-muted-foreground">
                        icon(data: iconify_icon!("lucide:info"), size: 16, attrs: attributes! { class="mt-0.5 shrink-0" })
                        <span>
                            "If the app has migrations, run them in the "
                            <a href=(&advanced_url) class="font-medium text-foreground underline underline-offset-4">"script after the build"</a>
                            ", in the Advanced tab. It runs after each build and before the new release starts, with the databases already linked."
                        </span>
                    </p>
                    button(
                        size: ButtonSize::Sm,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open_adding.set(true)) },
                        icon(data: iconify_icon!("lucide:plus"))
                        "Add database"
                    )
                </div>
                if entries.is_empty() {
                    <p class="rounded-lg border border-dashed border-border p-4 text-sm text-muted-foreground">
                        "No database yet. Add one, or it shows up here once the app creates it in the storage folder."
                    </p>
                } else {
                    #[key(entry.file.clone())]
                    for (index, entry) in entries.iter().enumerate() {
                        database_card(
                            file: &entry.file,
                            managed: entry.managed.map(|at| &managed[at]),
                            found: entry.found.map(|at| &databases[at]),
                            files: &files,
                            urls: &urls,
                            first: &entry_dialogs[index].0,
                            second: &entry_dialogs[index].1
                        )
                    }
                }
            )
            settings_card(
                title: "Backups",
                text: "Copies of the databases. Restoring one stops the app, backs up the current database, puts the copy in its place and starts the app again.",
                if backups.is_empty() {
                    <p class="text-sm text-muted-foreground">"No backups yet. Back up a database above."</p>
                } else {
                    <ul class="divide-y divide-border rounded-lg border border-border">
                        #[key((backup.database.clone(), backup.name.clone()))]
                        for (index, backup) in backups.iter().enumerate() {
                            backup_row(backup: backup, urls: &urls, restoring: &dialogs[index].0, deleting: &dialogs[index].1)
                        }
                    </ul>
                }
            )
            if !others.is_empty() {
                settings_card(
                    title: "Other files",
                    text: "Everything else in the storage folder.",
                    <ul class="divide-y divide-border rounded-lg border border-border text-sm">
                        #[key(file.name.clone())]
                        for file in &others {
                            other_file(file: file)
                        }
                    </ul>
                )
            }
        </div>
        #[key((backup.database.clone(), backup.name.clone()))]
        for (index, backup) in backups.iter().enumerate() {
            restore_dialog(open: &dialogs[index].0, backup: backup, action: &urls.restore)
            delete_dialog(open: &dialogs[index].1, backup: backup, action: &urls.delete)
        }
        database_dialog(open: &adding, action: &urls.add, variable: suggested_variable)
        #[key(entry.file.clone())]
        for (index, entry) in entries.iter().enumerate() {
            match entry.managed {
                None => {
                    database_dialog(
                        open: &entry_dialogs[index].0,
                        action: &urls.add,
                        existing: Some(entry.file.as_str()),
                        variable: suggested_variable
                    )
                }
                Some(at) => {
                    disconnect_dialog(open: &entry_dialogs[index].0, database: &managed[at], action: &urls.disconnect)
                    remove_database_dialog(open: &entry_dialogs[index].1, database: &managed[at], action: &urls.delete_database)
                }
            }
        }
    })
}

/// A database in the storage folder: one oxploy manages, one found in the folder, or
/// both, by its place in each list.
struct Entry {
    file: String,
    managed: Option<usize>,
    found: Option<usize>,
}

/// Where the tab's forms and links go.
struct Urls {
    back_up: String,
    restore: String,
    delete: String,
    download: String,
    add: String,
    disconnect: String,
    delete_database: String,
}

impl Urls {
    /// The link that downloads a database, or one of its backups.
    fn download(&self, database: &str, backup: Option<&str>) -> String {
        let mut url = format!("{}?database={}", self.download, encode(database));
        if let Some(backup) = backup {
            url += &format!("&backup={}", encode(backup));
        }
        url
    }
}

/// One database: whether oxploy manages it and how it reaches the app, its size and
/// tables, and its buttons. `first` and `second` open its dialogs: managing one found in
/// the folder, or disconnecting and deleting a managed one.
#[component]
async fn database_card(
    file: &str,
    managed: Option<&ManagedDatabase>,
    found: Option<&Database>,
    files: &AppFiles,
    urls: &Urls,
    first: &Signal<bool>,
    second: &Signal<bool>,
) -> Result<impl View> {
    let (first, second) = (first.clone(), second.clone());
    let summary = match found {
        Some(found) => {
            let tables = match &found.tables {
                Ok(tables) if tables.len() == 1 => "1 table".to_owned(),
                Ok(tables) => format!("{} tables", tables.len()),
                Err(_) => "tables unreadable".to_owned(),
            };
            format!("{} · {tables}", format_bytes(found.size))
        }
        None => "Not created yet".to_owned(),
    };

    Ok(view! {
        <div class="flex flex-col gap-3 rounded-lg border border-border p-4">
            <div class="flex flex-wrap items-center gap-x-3 gap-y-2">
                icon(data: iconify_icon!("lucide:database"), size: 18, attrs: attributes! { class="text-primary" })
                <span class="font-mono font-medium">(file)</span>
                <span class="text-sm text-muted-foreground">(summary)</span>
                if managed.is_some() {
                    <span class="rounded-full bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary">"Managed"</span>
                } else {
                    <span class="rounded-full border border-border px-2 py-0.5 text-xs text-muted-foreground">"Found in the folder"</span>
                }
                <div class="ml-auto flex flex-wrap items-center gap-2">
                    if found.is_some() {
                        <a
                            href=(urls.download(file, None))
                            class=(button_variants(ButtonVariant::Ghost, ButtonSize::Icon))
                            title="Download"
                            aria-label="Download"
                        >
                            icon(data: iconify_icon!("lucide:download"))
                        </a>
                        <form method="post" action=(&urls.back_up)>
                            <input type="hidden" name="database" value=(file)>
                            button(
                                variant: ButtonVariant::Outline,
                                size: ButtonSize::Sm,
                                attrs: attributes! { type="submit" },
                                icon(data: iconify_icon!("lucide:archive"))
                                "Back up"
                            )
                        </form>
                    }
                    if managed.is_some() {
                        button(
                            variant: ButtonVariant::Outline,
                            size: ButtonSize::Sm,
                            attrs: attributes! { type="button" @click=$(|_e: Event| first.set(true)) },
                            icon(data: iconify_icon!("lucide:unplug"))
                            "Disconnect"
                        )
                        button(
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::Icon,
                            attrs: attributes! {
                                type="button"
                                class="hover:text-destructive"
                                title="Delete"
                                aria-label="Delete"
                                @click=$(|_e: Event| second.set(true))
                            },
                            icon(data: iconify_icon!("lucide:trash-2"))
                        )
                    } else {
                        button(
                            size: ButtonSize::Sm,
                            attrs: attributes! { type="button" @click=$(|_e: Event| first.set(true)) },
                            icon(data: iconify_icon!("lucide:plug"))
                            "Manage"
                        )
                    }
                </div>
            </div>
            if let Some(database) = managed {
                <dl class="grid grid-cols-1 gap-x-6 gap-y-1.5 text-sm sm:grid-cols-[6rem_minmax(0,1fr)]">
                    <dt class="text-muted-foreground">"Variable"</dt>
                    <dd class="font-mono break-all">
                        match &database.variable {
                            Some(variable) => { (variable) "=" (database.value(files)) }
                            None => { <span class="font-sans text-muted-foreground">"None"</span> }
                        }
                    </dd>
                    <dt class="text-muted-foreground">"Link"</dt>
                    <dd class="font-mono break-all">
                        match &database.link {
                            Some(link) => { (link) " → storage/" (&database.file) }
                            None => { <span class="font-sans text-muted-foreground">"None"</span> }
                        }
                    </dd>
                </dl>
            }
            if let Some(found) = found {
                match &found.tables {
                    Ok(tables) => {
                        if !tables.is_empty() {
                            <ul class="grid grid-cols-1 gap-x-8 gap-y-1 text-sm sm:grid-cols-2 lg:grid-cols-3">
                                #[key(name.clone())]
                                for (name, rows) in tables {
                                    <li class="flex min-w-0 items-center justify-between gap-3">
                                        <span class="truncate font-mono">(name)</span>
                                        <span class="text-muted-foreground tabular-nums">
                                            (match rows { 1 => "1 row".to_owned(), rows => format!("{rows} rows") })
                                        </span>
                                    </li>
                                }
                            </ul>
                        }
                    }
                    Err(problem) => {
                        <p class="text-sm text-destructive">(problem)</p>
                    }
                }
            }
        </div>
    })
}

/// The form that adds a database, or starts managing `existing`, one already in the
/// storage folder. `variable` is the variable to suggest.
#[component]
async fn database_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    action: &str,
    #[default] existing: Option<&str>,
    variable: &str,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let name_id = element_id(cx);
    let variable_id = element_id(cx);
    let format_id = element_id(cx);
    let link_id = element_id(cx);

    Ok(view! {
        dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(
                        attrs: attributes! { id=(&title_id) },
                        match existing {
                            Some(file) => { "Manage " (file) }
                            None => { "Add database" }
                        }
                    )
                    dialog_description(
                        match existing {
                            Some(_) => "oxploy connects this database to the app the next time it starts.",
                            None => "oxploy creates the database in the storage folder and connects it to the app the next time it starts.",
                        }
                    )
                )
                <form method="post" action=(action) class="flex flex-col gap-4">
                    field(
                        field_label("Type")
                        select(
                            attrs: attributes! { name="type" },
                            <option value="sqlite" selected="">"SQLite"</option>
                        )
                    )
                    match existing {
                        Some(file) => {
                            <input type="hidden" name="file" value=(file)>
                        }
                        None => {
                            field(
                                field_label(attrs: attributes! { for=(&name_id) }, "Name")
                                input(
                                    attrs: attributes! {
                                        id=(&name_id)
                                        name="file"
                                        placeholder="data"
                                        pattern="[A-Za-z0-9_\\-]+"
                                        maxlength="64"
                                        autocomplete="off"
                                        required=""
                                    }
                                )
                                field_description("Saved as the name and .db in the storage folder.")
                            )
                        }
                    }
                    field(
                        field_label(attrs: attributes! { for=(&variable_id) }, "Variable")
                        input(
                            attrs: attributes! {
                                id=(&variable_id)
                                name="variable"
                                value=(variable)
                                placeholder="Optional"
                                maxlength="128"
                                autocomplete="off"
                            }
                        )
                        field_description("oxploy sets it to the database for the app, its builds and the script after the build.")
                    )
                    field(
                        field_label(attrs: attributes! { for=(&format_id) }, "Value")
                        select(
                            attrs: attributes! { id=(&format_id) name="format" },
                            <option value="sqlite_url" selected="">"sqlite:/…/file.db, for toasty and sqlx"</option>
                            <option value="plain_path">"/…/file.db, for rusqlite and diesel"</option>
                        )
                    )
                    field(
                        field_label(attrs: attributes! { for=(&link_id) }, "Link")
                        input(
                            attrs: attributes! {
                                id=(&link_id)
                                name="link"
                                value=(existing.unwrap_or(""))
                                placeholder="Optional, such as data.db"
                                maxlength="255"
                                autocomplete="off"
                            }
                        )
                        field_description("Where the app opens the file, from the build path. oxploy puts a link there in each release, for apps with the path in their code.")
                    )
                    dialog_footer(
                        button(
                            variant: ButtonVariant::Ghost,
                            attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                            "Cancel"
                        )
                        button(
                            attrs: attributes! { type="submit" },
                            (if existing.is_some() { "Manage" } else { "Create database" })
                        )
                    )
                </form>
            )
        )
    })
}

/// A dialog that confirms no longer managing a database.
#[component]
async fn disconnect_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    database: &ManagedDatabase,
    action: &str,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);

    Ok(view! {
        alert_dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Disconnect " (&database.file))
                    dialog_description(
                        "The file stays in the storage folder, but the app no longer gets its variable or link the next time it starts."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(action)>
                        <input type="hidden" name="id" value=(database.id.to_string())>
                        button(attrs: attributes! { type="submit" }, "Disconnect")
                    </form>
                )
            )
        )
    })
}

/// A dialog that confirms deleting a managed database.
#[component]
async fn remove_database_dialog(
    cx: &Cx,
    open: &Signal<bool>,
    database: &ManagedDatabase,
    action: &str,
) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);

    Ok(view! {
        alert_dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Delete " (&database.file))
                    dialog_description(
                        "oxploy stops the app, backs up the database, deletes it with its link, and starts the app again. The backup stays under Backups."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(action)>
                        <input type="hidden" name="id" value=(database.id.to_string())>
                        button(variant: ButtonVariant::Destructive, attrs: attributes! { type="submit" }, "Delete")
                    </form>
                )
            )
        )
    })
}

/// One backup, with the buttons that download, restore and delete it.
#[component]
async fn backup_row(
    backup: &Backup,
    urls: &Urls,
    restoring: &Signal<bool>,
    deleting: &Signal<bool>,
) -> Result<impl View> {
    let (restoring, deleting) = (restoring.clone(), deleting.clone());

    Ok(view! {
        <li class="flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3">
            <div class="flex min-w-0 flex-1 flex-col">
                <span class="truncate font-mono text-sm">(&backup.name)</span>
                <span class="text-xs text-muted-foreground">
                    "of " <span class="font-mono">(&backup.database)</span>
                    " · " (format_bytes(backup.size)) " · made " created_at(seconds: backup.made_at)
                </span>
            </div>
            <div class="flex items-center gap-1">
                <a
                    href=(urls.download(&backup.database, Some(&backup.name)))
                    class=(button_variants(ButtonVariant::Ghost, ButtonSize::Icon))
                    title="Download"
                    aria-label="Download"
                >
                    icon(data: iconify_icon!("lucide:download"))
                </a>
                button(
                    variant: ButtonVariant::Outline,
                    size: ButtonSize::Sm,
                    attrs: attributes! { type="button" @click=$(|_e: Event| restoring.set(true)) },
                    icon(data: iconify_icon!("lucide:history"))
                    "Restore"
                )
                button(
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Icon,
                    attrs: attributes! {
                        type="button"
                        class="hover:text-destructive"
                        title="Delete"
                        aria-label="Delete"
                        @click=$(|_e: Event| deleting.set(true))
                    },
                    icon(data: iconify_icon!("lucide:trash-2"))
                )
            </div>
        </li>
    })
}

/// A file or folder in the storage folder that is not a database.
#[component]
async fn other_file(file: &StoredFile) -> Result<impl View> {
    Ok(view! {
        <li class="flex items-center gap-3 px-4 py-2.5">
            if file.is_dir {
                icon(data: iconify_icon!("lucide:folder"), size: 16, attrs: attributes! { class="text-muted-foreground" })
            } else {
                icon(data: iconify_icon!("lucide:file"), size: 16, attrs: attributes! { class="text-muted-foreground" })
            }
            <span class="min-w-0 flex-1 truncate font-mono">(&file.name)</span>
            <span class="text-muted-foreground tabular-nums">(format_bytes(file.size))</span>
        </li>
    })
}

/// A dialog that confirms putting a backup in place of its database.
#[component]
async fn restore_dialog(cx: &Cx, open: &Signal<bool>, backup: &Backup, action: &str) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let description_id = element_id(cx);

    Ok(view! {
        alert_dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) aria-describedby=(&description_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Restore " (&backup.database))
                    dialog_description(
                        attrs: attributes! { id=(&description_id) },
                        "oxploy stops the app, backs up the current "
                        <strong class="font-medium text-foreground">(&backup.database)</strong>
                        ", puts the copy from "
                        <strong class="font-medium text-foreground">(&backup.name)</strong>
                        " in its place, and starts the app again. Whatever the app wrote since that copy is only in the new backup."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(action)>
                        backup_fields(backup: backup)
                        button(attrs: attributes! { type="submit" }, "Restore")
                    </form>
                )
            )
        )
    })
}

/// A dialog that confirms deleting a backup.
#[component]
async fn delete_dialog(cx: &Cx, open: &Signal<bool>, backup: &Backup, action: &str) -> Result<impl View> {
    let open = open.clone();
    let title_id = element_id(cx);
    let description_id = element_id(cx);

    Ok(view! {
        alert_dialog(
            open: $(open.get()),
            attrs: attributes! { aria-labelledby=(&title_id) aria-describedby=(&description_id) },
            dialog_content(
                dialog_header(
                    dialog_title(attrs: attributes! { id=(&title_id) }, "Delete backup")
                    dialog_description(
                        attrs: attributes! { id=(&description_id) },
                        "This deletes the backup "
                        <strong class="font-medium text-foreground">(&backup.name)</strong>
                        " of " (&backup.database) ". It cannot be undone."
                    )
                )
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { type="button" @click=$(|_e: Event| open.set(false)) },
                        "Cancel"
                    )
                    <form method="post" action=(action)>
                        backup_fields(backup: backup)
                        button(variant: ButtonVariant::Destructive, attrs: attributes! { type="submit" }, "Delete")
                    </form>
                )
            )
        )
    })
}

/// The fields that name a backup in a form.
#[component]
async fn backup_fields(backup: &Backup) -> Result<impl View> {
    Ok(view! {
        <input type="hidden" name="database" value=(&backup.database)>
        <input type="hidden" name="backup" value=(&backup.name)>
    })
}

/// Percent-encodes a path or file name for a query string.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// A form that names a database.
#[derive(Deserialize)]
struct DatabaseForm {
    database: String,
}

/// A form that names a backup of a database.
#[derive(Deserialize)]
struct BackupForm {
    database: String,
    backup: String,
}

/// Backs up a database, then shows the tab again.
#[route(POST "./back-up")]
async fn back_up(cx: &Cx, Form(form): Form<DatabaseForm>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    let files = AppFiles::of(&service.app_name);
    let backed_up = tokio::task::spawn_blocking(move || storage::back_up(&files, &form.database, ""))
        .await?;
    if let Err(error) = backed_up {
        return Err(bad_request(format!("The database could not be backed up: {error}")).into());
    }
    Ok(see_other(ServiceTab::Storage.url(cx, &project, &service)))
}

/// Puts a backup in place of its database, then shows the tab again.
#[route(POST "./restore")]
async fn restore(cx: &Cx, Form(form): Form<BackupForm>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    if let Err(problem) = Deployer::get(cx)
        .restore(&service, &form.database, &form.backup)
        .await?
    {
        return Err(bad_request(format!("The backup could not be restored: {problem}")).into());
    }
    Ok(see_other(ServiceTab::Storage.url(cx, &project, &service)))
}

/// Deletes a backup, then shows the tab again.
#[route(POST "./delete-backup")]
async fn delete_backup(cx: &Cx, Form(form): Form<BackupForm>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    let files = AppFiles::of(&service.app_name);
    storage::delete_backup(&files, &form.database, &form.backup)
        .map_err(|error| bad_request(error.to_string()))?;
    Ok(see_other(ServiceTab::Storage.url(cx, &project, &service)))
}

/// What to download: a database, or one of its backups.
#[query_params(error = bad_request)]
struct DownloadQuery {
    database: String,
    backup: Option<String>,
}

/// Sends a database, as a consistent copy, or one of its backups.
#[route(GET "./download")]
async fn download(cx: &Cx) -> Result<([(HeaderName, String); 2], Vec<u8>)> {
    let (_, service) = current_service(cx).await?;
    let query = query_params::<DownloadQuery>(cx)?;
    let files = AppFiles::of(&service.app_name);
    let database_name = query.database.rsplit('/').next().unwrap_or("database").to_owned();
    let (bytes, name) = match &query.backup {
        Some(backup) => {
            let file = storage::backup_file(&files, &query.database, backup).ok_or_not_found()?;
            let stem = database_name.strip_suffix(".db").unwrap_or(&database_name);
            (tokio::fs::read(file).await?, format!("{stem}-{backup}"))
        }
        None => {
            let database = query.database.clone();
            let copied = tokio::task::spawn_blocking(move || storage::snapshot(&files, &database))
                .await?;
            (copied.map_err(|_| topcoat::router::error::not_found())?, database_name)
        }
    };
    let name = name.replace('"', "");
    Ok((
        [
            (CONTENT_TYPE, "application/vnd.sqlite3".to_owned()),
            (CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        bytes,
    ))
}

/// A form that adds a database, or starts managing one in the storage folder.
#[derive(Deserialize)]
struct AddDatabaseForm {
    file: String,
    #[serde(default)]
    variable: String,
    format: String,
    #[serde(default)]
    link: String,
}

/// Adds a database, or starts managing one in the storage folder.
#[route(POST "./databases")]
async fn add_database(cx: &Cx, Form(form): Form<AddDatabaseForm>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    let format = match form.format.as_str() {
        "plain_path" => ValueFormat::PlainPath,
        _ => ValueFormat::SqliteUrl,
    };
    let new = NewDatabase {
        file: &form.file,
        variable: &form.variable,
        format,
        link: &form.link,
    };
    if let Err(problem) = ManagedDatabase::add(cx, &service, &new).await? {
        return Err(bad_request(problem).into());
    }
    Ok(see_other(ServiceTab::Storage.url(cx, &project, &service)))
}

/// A form that names a managed database.
#[derive(Deserialize)]
struct ManagedForm {
    id: u64,
}

/// Stops managing a database, keeping its file.
#[route(POST "./databases/disconnect")]
async fn disconnect_database(cx: &Cx, Form(form): Form<ManagedForm>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    let database = ManagedDatabase::find_of(cx, service.id, form.id)
        .await?
        .ok_or_not_found()?;
    if let Err(problem) = database.disconnect(cx, &service).await? {
        return Err(bad_request(format!("The database could not be disconnected: {problem}")).into());
    }
    Ok(see_other(ServiceTab::Storage.url(cx, &project, &service)))
}

/// Deletes a managed database after backing it up.
#[route(POST "./databases/delete")]
async fn delete_database(cx: &Cx, Form(form): Form<ManagedForm>) -> Result<SeeOther> {
    let (project, service) = current_service(cx).await?;
    let database = ManagedDatabase::find_of(cx, service.id, form.id)
        .await?
        .ok_or_not_found()?;
    if let Err(problem) = Deployer::get(cx).delete_database(&service, database).await? {
        return Err(bad_request(format!("The database could not be deleted: {problem}")).into());
    }
    Ok(see_other(ServiceTab::Storage.url(cx, &project, &service)))
}
