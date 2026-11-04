//! The time zone the panel shows times in, which the super admin picks in Settings: its
//! clock, its charts and dates, and the times oxploy writes in the apps' logs and in their
//! backups' names. Until one is picked, it is the server's own. The apps keep the
//! server's own either way.

use std::sync::{LazyLock, RwLock};

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use toasty::Db;
use topcoat::{Result, context::Cx};

use super::project::now as seconds_now;
use crate::db::db;

/// The time zone the super admin picked. There is at most one.
#[derive(Debug, toasty::Model)]
pub struct PanelTimeZone {
    #[key]
    #[auto]
    pub id: u64,

    /// Its name in the time zone database, such as `America/Sao_Paulo`.
    pub name: String,

    /// When it was picked, in seconds since the Unix epoch.
    pub created_at: i64,
}

/// The time zone in use, which the database sets at start and Settings changes.
static CURRENT: LazyLock<RwLock<TimeZone>> = LazyLock::new(|| RwLock::new(TimeZone::system()));

/// The time zone the panel shows times in.
pub fn current() -> TimeZone {
    CURRENT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The time now, in the panel's time zone.
pub fn now() -> Zoned {
    Zoned::now().with_time_zone(current())
}

/// `timestamp` in the panel's time zone.
pub fn at(timestamp: Timestamp) -> Zoned {
    timestamp.to_zoned(current())
}

/// The name of the server's own time zone, such as `UTC`.
pub fn server_zone() -> String {
    TimeZone::system()
        .iana_name()
        .unwrap_or("UTC")
        .to_owned()
}

/// The time zones that can be picked, by name, in order. Old names without a region,
/// such as `Cuba`, are left out, but not `UTC`.
pub fn names() -> Vec<String> {
    let mut names: Vec<String> = jiff::tz::db()
        .available()
        .map(|name| name.to_string())
        .filter(|name| {
            (name.contains('/') && !name.starts_with("posix/") && !name.starts_with("right/"))
                || name == "UTC"
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

fn set_current(zone: TimeZone) {
    *CURRENT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = zone;
}

impl PanelTimeZone {
    /// Takes up the picked time zone, as the panel starts.
    pub async fn load(db: &Db) -> toasty::Result<()> {
        let picked = PanelTimeZone::all().first().exec(&mut db.clone()).await?;
        // A name the server's database no longer has leaves the server's own.
        if let Some(zone) = picked.and_then(|picked| jiff::tz::db().get(&picked.name).ok()) {
            set_current(zone);
        }
        Ok(())
    }

    /// The name of the picked time zone, if one is picked.
    pub async fn picked(cx: &Cx) -> Result<Option<String>> {
        let picked = PanelTimeZone::all().first().exec(&mut db(cx)).await?;
        Ok(picked.map(|picked| picked.name))
    }

    /// Picks the time zone named `name`, or the server's own when it is empty. Returns
    /// `false`, changing nothing, if there is no time zone by that name.
    pub async fn pick(cx: &Cx, name: &str) -> Result<bool> {
        let name = name.trim();
        let zone = if name.is_empty() {
            None
        } else {
            match jiff::tz::db().get(name) {
                Ok(zone) => Some(zone),
                Err(_) => return Ok(false),
            }
        };
        PanelTimeZone::all().delete().exec(&mut db(cx)).await?;
        match zone {
            Some(zone) => {
                toasty::create!(PanelTimeZone {
                    name: name.to_owned(),
                    created_at: seconds_now(),
                })
                .exec(&mut db(cx))
                .await?;
                set_current(zone);
            }
            None => set_current(TimeZone::system()),
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_are_named_by_region() {
        let names = names();
        assert!(names.iter().any(|name| name == "America/Sao_Paulo"));
        assert!(names.iter().any(|name| name == "UTC"));
        assert!(!names.iter().any(|name| name.starts_with("posix/") || name == "Cuba"));
    }
}
