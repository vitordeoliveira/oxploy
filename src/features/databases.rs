//! Databases oxploy manages for a service: SQLite files in its storage folder that it
//! creates, links into each release and connects to the app through a variable.
//!
//! The link serves apps that open a fixed path in their code, such as `sqlite:data.db`;
//! the variable serves apps that read one, such as `DATABASE_URL`. A service can use
//! either or both, since they lead to the same file.

use std::fs;
use std::io;
use std::path::Path;

use rusqlite::Connection;
use toasty::Db;
use topcoat::{Result, context::Cx};

use super::app_files::AppFiles;
use super::environment::{self, PORT_VARIABLE, SetElsewhere};
use super::project::now;
use super::service::Service;
use super::storage;
use crate::db::db;

/// A SQLite database oxploy manages for a service.
#[derive(Debug, toasty::Model)]
pub struct ManagedDatabase {
    #[key]
    #[auto]
    pub id: u64,

    #[index]
    pub service_id: u64,

    /// Its path in the storage folder, such as `data.db`.
    pub file: String,

    /// The variable oxploy sets to it for the app, such as `DATABASE_URL`.
    pub variable: Option<String>,

    /// How the variable's value is written.
    pub format: ValueFormat,

    /// Where oxploy links it in each release, from the build path, such as `data.db`.
    pub link: Option<String>,

    /// When oxploy started managing it, in seconds since the Unix epoch.
    pub created_at: i64,
}

/// How a database's variable is written, which depends on the library the app opens
/// it with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, toasty::Embed)]
pub enum ValueFormat {
    /// `sqlite:` and the path, as toasty and sqlx read it.
    SqliteUrl,
    /// The path alone, as rusqlite and diesel read it.
    PlainPath,
}

/// A database to manage, as the user filled the form.
pub struct NewDatabase<'a> {
    /// A name for a new database, which becomes `<name>.db`, or the path of a database
    /// already in the storage folder.
    pub file: &'a str,
    pub variable: &'a str,
    pub format: ValueFormat,
    pub link: &'a str,
}

/// The most characters a new database's name may have.
pub const MAX_NAME_LEN: usize = 64;

impl ManagedDatabase {
    /// Returns the databases oxploy manages for a service, oldest first.
    pub async fn of_service(cx: &Cx, service_id: u64) -> Result<Vec<ManagedDatabase>> {
        Ok(Self::of_service_in(&db(cx), service_id).await?)
    }

    /// Like [`of_service`](Self::of_service), for work done outside a request.
    pub async fn of_service_in(db: &Db, service_id: u64) -> toasty::Result<Vec<ManagedDatabase>> {
        ManagedDatabase::filter_by_service_id(service_id)
            .order_by(ManagedDatabase::fields().id().asc())
            .exec(&mut db.clone())
            .await
    }

    /// Returns the managed database with this ID if it belongs to the service.
    pub async fn find_of(cx: &Cx, service_id: u64, id: u64) -> Result<Option<ManagedDatabase>> {
        let database = ManagedDatabase::filter_by_id(id)
            .first()
            .exec(&mut db(cx))
            .await?;
        Ok(database.filter(|database| database.service_id == service_id))
    }

    /// Starts managing a database for the service: a new one, created empty, or one
    /// already in its storage folder. Returns what is wrong instead, if a field is
    /// invalid or taken.
    pub async fn add(
        cx: &Cx,
        service: &Service,
        new: &NewDatabase<'_>,
    ) -> Result<Result<ManagedDatabase, String>> {
        let files = AppFiles::of(&service.app_name);
        let managed = Self::of_service(cx, service.id).await?;
        let (file, variable, link) = match check(service, &files, &managed, new) {
            Ok(fields) => fields,
            Err(problem) => return Ok(Err(problem)),
        };
        let path = files.storage().join(&file);
        if !path.exists() {
            let created = (|| -> io::Result<()> {
                if let Some(folder) = path.parent() {
                    fs::create_dir_all(folder)?;
                }
                create_empty(&path).map_err(io::Error::other)
            })();
            if let Err(error) = created {
                return Ok(Err(format!("the database could not be created: {error}")));
            }
        }
        let database = toasty::create!(ManagedDatabase {
            service_id: service.id,
            file,
            variable,
            format: new.format,
            link,
            created_at: now(),
        })
        .exec(&mut db(cx))
        .await?;
        Ok(Ok(database))
    }

    /// Stops managing the database. Its file stays in the storage folder. Returns why
    /// not instead, if the service's own variables use the database's.
    pub async fn disconnect(self, cx: &Cx, service: &Service) -> Result<Result<(), String>> {
        let managed = Self::of_service(cx, service.id).await?;
        if let Some(problem) = still_used(service, &managed, &self) {
            return Ok(Err(problem));
        }
        ManagedDatabase::filter_by_id(self.id)
            .delete()
            .exec(&mut db(cx))
            .await?;
        Ok(Ok(()))
    }

    /// Stops managing every database of a service, before the service is deleted.
    pub async fn disconnect_all(db: &Db, service_id: u64) -> toasty::Result<()> {
        ManagedDatabase::filter_by_service_id(service_id)
            .delete()
            .exec(&mut db.clone())
            .await
    }

    /// The value of the database's variable for an app whose files are `files`.
    pub fn value(&self, files: &AppFiles) -> String {
        let path = files.storage().join(&self.file).display().to_string();
        match self.format {
            ValueFormat::SqliteUrl => format!("sqlite:{path}"),
            ValueFormat::PlainPath => path,
        }
    }
}

/// The variables a service's databases set, with their values.
pub fn variables(databases: &[ManagedDatabase], files: &AppFiles) -> Vec<(String, String)> {
    databases
        .iter()
        .filter_map(|database| Some((database.variable.clone()?, database.value(files))))
        .collect()
}

/// Returns why `leaving` cannot stop being managed, if the service's own variables use
/// its variable: without it, they would no longer read.
pub fn still_used(
    service: &Service,
    managed: &[ManagedDatabase],
    leaving: &ManagedDatabase,
) -> Option<String> {
    let variable = leaving.variable.as_deref()?;
    let remaining = set_elsewhere(managed.iter().filter(|database| database.id != leaving.id));
    environment::parse(service.environment.as_deref().unwrap_or(""), &remaining)
        .err()
        .map(|_| format!("the Environment tab uses ${{{variable}}}; remove it there first"))
}

/// The variables a service's databases set, for the Environment tab to refuse.
pub fn set_elsewhere<'a>(
    databases: impl IntoIterator<Item = &'a ManagedDatabase>,
) -> Vec<SetElsewhere<'a>> {
    databases
        .into_iter()
        .filter_map(|database| {
            Some(SetElsewhere {
                name: database.variable.as_deref()?,
                by: format!("the database {} in the Storage tab", database.file),
            })
        })
        .collect()
}

/// Puts a link to each database at its link path under `work_dir`, the folder a
/// release runs in, replacing a file the code had there. Returns the links made.
pub fn link_all(
    work_dir: &Path,
    files: &AppFiles,
    databases: &[ManagedDatabase],
) -> io::Result<Vec<String>> {
    let mut linked = Vec::new();
    for database in databases {
        let Some(link) = &database.link else {
            continue;
        };
        let at = work_dir.join(link);
        if let Some(folder) = at.parent() {
            fs::create_dir_all(folder)?;
        }
        match fs::symlink_metadata(&at) {
            Ok(metadata) if metadata.is_dir() => {
                return Err(io::Error::other(format!("{link} is a folder in the code")));
            }
            Ok(_) => fs::remove_file(&at)?,
            Err(_) => {}
        }
        std::os::unix::fs::symlink(files.storage().join(&database.file), &at)?;
        linked.push(link.clone());
    }
    Ok(linked)
}

/// Checks a database to manage, and returns its file, variable and link.
fn check(
    service: &Service,
    files: &AppFiles,
    managed: &[ManagedDatabase],
    new: &NewDatabase<'_>,
) -> Result<(String, Option<String>, Option<String>), String> {
    let wanted = new.file.trim();
    // A plain name is a new database; a path ending in `.db` or with a folder names
    // one already in the storage folder.
    let file = if storage::is_relative_path(wanted)
        && files.storage().join(wanted).is_file()
    {
        if !storage::is_database(&files.storage().join(wanted)) {
            return Err(format!("{wanted} is not a SQLite database"));
        }
        wanted.to_owned()
    } else if !wanted.is_empty()
        && wanted.len() <= MAX_NAME_LEN
        && wanted
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        format!("{wanted}.db")
    } else {
        return Err("Name the database with letters, digits, - and _".to_owned());
    };
    if managed.iter().any(|database| database.file == file) {
        return Err(format!("{file} is already managed"));
    }

    let variable = new.variable.trim();
    let variable = if variable.is_empty() {
        None
    } else {
        if !environment::is_key(variable) {
            return Err(format!("{variable} is not a variable name"));
        }
        if variable == PORT_VARIABLE || variable.starts_with("OXPLOY_") {
            return Err(format!("{variable} is set by oxploy"));
        }
        if managed
            .iter()
            .any(|database| database.variable.as_deref() == Some(variable))
        {
            return Err(format!("another database already sets {variable}"));
        }
        let own = environment::parse(service.environment.as_deref().unwrap_or(""), &[])
            .unwrap_or_default();
        if own.iter().any(|set| set.key == variable) {
            return Err(format!(
                "{variable} is already in the Environment tab; remove it there first"
            ));
        }
        Some(variable.to_owned())
    };

    let link = new.link.trim().trim_start_matches('/');
    let link = if link.is_empty() {
        None
    } else {
        if !storage::is_relative_path(link) {
            return Err(format!("{link} is not a path inside the code"));
        }
        if managed
            .iter()
            .any(|database| database.link.as_deref() == Some(link))
        {
            return Err(format!("another database is already linked at {link}"));
        }
        Some(link.to_owned())
    };
    Ok((file, variable, link))
}

/// Creates an empty SQLite database. SQLite writes nothing to a new file until its first
/// change, so one is made and undone, leaving a file other tools recognise.
fn create_empty(path: &Path) -> rusqlite::Result<()> {
    let connection = Connection::open(path)?;
    connection.execute_batch("CREATE TABLE oxploy_new (x); DROP TABLE oxploy_new; VACUUM;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_follow_the_library() {
        let files = AppFiles::of("shop");
        let database = |format| ManagedDatabase {
            id: 1,
            service_id: 1,
            file: "data.db".to_owned(),
            variable: Some("DATABASE_URL".to_owned()),
            format,
            link: None,
            created_at: 0,
        };
        let path = files.storage().join("data.db").display().to_string();
        assert_eq!(database(ValueFormat::SqliteUrl).value(&files), format!("sqlite:{path}"));
        assert_eq!(database(ValueFormat::PlainPath).value(&files), path);
    }
}
