//! What a service keeps in its storage folder: SQLite databases, their backups, and
//! other files.
//!
//! Databases are found by the header every SQLite file starts with, so any database
//! the app creates in the folder shows up without being registered. Backups are made
//! with `VACUUM INTO`, which copies a consistent database even while the app writes to
//! it, and kept outside the storage folder, one folder per database.
//!
//! These functions read and write files, so callers run them off the async threads.

use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

use super::app_files::{AppFiles, size_of};
use super::time_zone;

/// The first bytes of every SQLite database file.
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// The files SQLite keeps next to a database while it is in use.
const COMPANIONS: [&str; 3] = ["-wal", "-shm", "-journal"];

/// How many folders deep databases are looked for in the storage folder.
const MAX_DEPTH: usize = 3;

/// How long to wait for the app to finish writing before reading a database.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// A SQLite database in the storage folder.
pub struct Database {
    /// Its path in the storage folder, such as `data.db` or `db/app.sqlite`.
    pub path: String,
    /// The bytes it takes, with the files SQLite keeps next to it.
    pub size: u64,
    /// Its tables with how many rows each has, or why they could not be read.
    pub tables: Result<Vec<(String, u64)>, String>,
}

/// A file or folder in the storage folder that is not a database.
pub struct StoredFile {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// A copy of a database.
pub struct Backup {
    /// The database it copies, as a path in the storage folder.
    pub database: String,
    /// Its file name, which says when it was made.
    pub name: String,
    pub size: u64,
    /// When it was made, in seconds since the Unix epoch.
    pub made_at: i64,
}

/// Something about a database or backup that went wrong.
#[derive(Debug)]
pub struct StorageError(String);

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StorageError {}

impl From<std::io::Error> for StorageError {
    fn from(error: std::io::Error) -> Self {
        StorageError(error.to_string())
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        StorageError(error.to_string())
    }
}

/// Returns the databases in the service's storage folder, by path.
pub fn databases(files: &AppFiles) -> Vec<Database> {
    let storage = files.storage();
    let mut found = Vec::new();
    find_databases(&storage, &storage, 0, &mut found);
    found.sort();
    found
        .into_iter()
        .map(|path| {
            let file = storage.join(&path);
            Database {
                size: with_companions(&file).iter().map(|file| file_size(file)).sum(),
                tables: tables(&file).map_err(|error| error.to_string()),
                path,
            }
        })
        .collect()
}

/// Returns what is in the storage folder besides its databases, by name. Folders are
/// listed whole, with what is in them.
pub fn other_files(files: &AppFiles) -> Vec<StoredFile> {
    let Ok(entries) = fs::read_dir(files.storage()) else {
        return Vec::new();
    };
    let mut others: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let path = entry.path();
            let is_dir = entry.file_type().ok()?.is_dir();
            let is_database_file = !is_dir
                && (is_database(&path)
                    || COMPANIONS
                        .iter()
                        .any(|suffix| name.strip_suffix(suffix).is_some_and(|base| {
                            is_database(&path.with_file_name(base))
                        })));
            (!is_database_file).then(|| StoredFile {
                size: if is_dir { size_of(&path) } else { file_size(&path) },
                name,
                is_dir,
            })
        })
        .collect();
    others.sort_by(|a, b| a.name.cmp(&b.name));
    others
}

/// Returns the backups of every database, newest first.
pub fn backups(files: &AppFiles) -> Vec<Backup> {
    let root = files.backups();
    let mut found = Vec::new();
    find_backups(&root, &root, 0, &mut found);
    found.sort_by(|a, b| b.made_at.cmp(&a.made_at).then_with(|| b.name.cmp(&a.name)));
    found
}

/// Copies a database in the storage folder into a new backup. `note` ends its name,
/// such as `-before-restore`.
pub fn back_up(files: &AppFiles, database: &str, note: &str) -> Result<Backup, StorageError> {
    let source = database_file(files, database)
        .filter(|file| is_database(file))
        .ok_or_else(|| StorageError(format!("there is no database {database} in the storage")))?;
    let folder = files.backups().join(database);
    fs::create_dir_all(&folder)?;
    let stamp = time_zone::now().strftime("%Y-%m-%d-%H%M%S").to_string();
    let mut name = format!("{stamp}{note}.db");
    for attempt in 2.. {
        if !folder.join(&name).exists() {
            break;
        }
        name = format!("{stamp}{note}-{attempt}.db");
    }
    let target = folder.join(&name);
    let connection = open(&source)?;
    connection.execute("VACUUM INTO ?1", [target.to_string_lossy()])?;
    Ok(Backup {
        database: database.to_owned(),
        size: file_size(&target),
        made_at: made_at(&target),
        name,
    })
}

/// Returns a consistent copy of a database in the storage folder, made the way backups
/// are, for downloading one that may be in use.
pub fn snapshot(files: &AppFiles, database: &str) -> Result<Vec<u8>, StorageError> {
    let source = database_file(files, database)
        .filter(|file| is_database(file))
        .ok_or_else(|| StorageError(format!("there is no database {database} in the storage")))?;
    let scratch = files.backups().join(".snapshots");
    fs::create_dir_all(&scratch)?;
    let target = scratch.join(format!("{}.db", jiff::Timestamp::now().as_nanosecond()));
    let copied = open(&source)
        .and_then(|connection| {
            connection.execute("VACUUM INTO ?1", [target.to_string_lossy()])?;
            Ok(())
        })
        .and_then(|()| Ok(fs::read(&target)?));
    let _ = fs::remove_file(&target);
    copied
}

/// Puts a backup in place of its database. The app must not be running, since the
/// database's files are replaced under it.
pub fn restore(files: &AppFiles, database: &str, name: &str) -> Result<(), StorageError> {
    let backup = backup_file(files, database, name)
        .ok_or_else(|| StorageError(format!("there is no backup {name} of {database}")))?;
    let target = database_file(files, database)
        .ok_or_else(|| StorageError(format!("{database} is not a path in the storage")))?;
    if let Some(folder) = target.parent() {
        fs::create_dir_all(folder)?;
    }
    // A write-ahead log left from the old database would be replayed into the new one.
    for companion in with_companions(&target).into_iter().skip(1) {
        let _ = fs::remove_file(companion);
    }
    let incoming = target.with_extension("restoring");
    fs::copy(&backup, &incoming)?;
    fs::rename(&incoming, &target)?;
    Ok(())
}

/// Deletes a backup.
pub fn delete_backup(files: &AppFiles, database: &str, name: &str) -> Result<(), StorageError> {
    let backup = backup_file(files, database, name)
        .ok_or_else(|| StorageError(format!("there is no backup {name} of {database}")))?;
    fs::remove_file(backup)?;
    Ok(())
}

/// Returns the database file at a path in the storage folder, if the path stays in it.
/// The file may not exist.
pub fn database_file(files: &AppFiles, database: &str) -> Option<PathBuf> {
    is_relative_path(database).then(|| files.storage().join(database))
}

/// Returns a backup's file, if it exists.
pub fn backup_file(files: &AppFiles, database: &str, name: &str) -> Option<PathBuf> {
    let valid = is_relative_path(database)
        && name.ends_with(".db")
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    let file = files.backups().join(database).join(name);
    (valid && file.is_file()).then_some(file)
}

/// Returns whether the file at `path` is a SQLite database.
pub fn is_database(path: &Path) -> bool {
    let mut header = [0; 16];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok_and(|()| &header == SQLITE_HEADER)
}

/// Adds the databases under `dir` to `found`, as paths relative to `storage`.
fn find_databases(storage: &Path, dir: &Path, depth: usize, found: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() && depth < MAX_DEPTH => {
                find_databases(storage, &path, depth + 1, found);
            }
            Ok(kind) if kind.is_file() && is_database(&path) => {
                if let Some(relative) = path.strip_prefix(storage).ok().and_then(Path::to_str) {
                    found.push(relative.to_owned());
                }
            }
            _ => {}
        }
    }
}

/// Adds the backups under `dir` to `found`. A backup's folder, relative to `root`, is
/// the path of the database it copies. Hidden folders, such as the one downloads are
/// copied through, are skipped.
fn find_backups(root: &Path, dir: &Path, depth: usize, found: &mut Vec<Backup>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
    {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() && depth <= MAX_DEPTH => {
                find_backups(root, &path, depth + 1, found);
            }
            Ok(kind) if kind.is_file() && depth > 0 => {
                let database = dir.strip_prefix(root).ok().and_then(Path::to_str);
                let name = entry.file_name().to_str().map(str::to_owned);
                if let (Some(database), Some(name)) = (database, name)
                    && name.ends_with(".db")
                {
                    found.push(Backup {
                        database: database.to_owned(),
                        size: file_size(&path),
                        made_at: made_at(&path),
                        name,
                    });
                }
            }
            _ => {}
        }
    }
}

/// Returns a database's tables with how many rows each has.
fn tables(file: &Path) -> Result<Vec<(String, u64)>, StorageError> {
    let connection = open(file)?;
    let names: Vec<String> = connection
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    names
        .into_iter()
        .map(|name| {
            let quoted = name.replace('"', "\"\"");
            let rows: i64 =
                connection.query_row(&format!("SELECT count(*) FROM \"{quoted}\""), [], |row| {
                    row.get(0)
                })?;
            Ok((name, u64::try_from(rows).unwrap_or(0)))
        })
        .collect()
}

/// Opens an existing database without creating one, waiting for the app's writes.
fn open(file: &Path) -> Result<Connection, StorageError> {
    let connection = Connection::open_with_flags(
        file,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    Ok(connection)
}

/// Returns a database file and the files SQLite keeps next to it, in that order.
pub fn with_companions(file: &Path) -> Vec<PathBuf> {
    let mut files = vec![file.to_path_buf()];
    files.extend(COMPANIONS.iter().map(|suffix| {
        let mut name = file.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    }));
    files
}

/// Returns whether `path` is a relative path that stays inside the folder it is joined
/// to: no `..`, no empty or hidden parts.
pub fn is_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 255
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.'))
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// When a file was last written, in seconds since the Unix epoch.
fn made_at(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_folder() {
        assert!(is_relative_path("data.db"));
        assert!(is_relative_path("db/app.sqlite"));
        assert!(!is_relative_path("../data.db"));
        assert!(!is_relative_path("/etc/passwd"));
        assert!(!is_relative_path("db/../../x"));
        assert!(!is_relative_path(".hidden"));
        assert!(!is_relative_path(""));
    }
}
