//! Services, the Rust apps a project deploys, stored in the database.

use tokio::sync::Mutex;
use topcoat::{Result, context::Cx};

use super::project::{Details, Project, now};
use crate::db::db;

/// A Rust app deployed from a repository.
#[derive(Debug, toasty::Model)]
pub struct Service {
    #[key]
    #[auto]
    pub id: u64,

    pub name: String,

    /// Names the app on the server, such as its container and the folders it builds
    /// in. Derived from the project and service names when the service is created, and
    /// fixed from then on.
    #[unique]
    pub app_name: String,

    pub description: Option<String>,

    #[index]
    pub project_id: u64,

    /// When the service was created, in seconds since the Unix epoch.
    pub created_at: i64,
}

/// The most characters an app name may have, which keeps it a valid DNS label.
const MAX_APP_NAME_LEN: usize = 63;

/// Serializes picking app names, so two services created at once cannot get the same
/// one.
static NAMING: Mutex<()> = Mutex::const_new(());

impl Service {
    /// Returns the services of a project, oldest first.
    pub async fn in_project(cx: &Cx, project_id: u64) -> Result<Vec<Service>> {
        let services = Service::filter_by_project_id(project_id)
            .order_by(Service::fields().id().asc())
            .exec(&mut db(cx))
            .await?;
        Ok(services)
    }

    /// Returns how many services a project has.
    pub async fn count_in(cx: &Cx, project_id: u64) -> Result<u64> {
        let count = Service::filter_by_project_id(project_id)
            .count()
            .exec(&mut db(cx))
            .await?;
        Ok(count)
    }

    /// Returns the service with this ID if it belongs to the project.
    pub async fn find_in(cx: &Cx, project_id: u64, id: u64) -> Result<Option<Service>> {
        let service = Service::filter_by_id(id).first().exec(&mut db(cx)).await?;
        Ok(service.filter(|service| service.project_id == project_id))
    }

    /// Creates a service in `project`. Returns `None`, creating nothing, if the details
    /// are invalid.
    pub async fn add(cx: &Cx, project: &Project, details: &Details<'_>) -> Result<Option<Service>> {
        let Some((name, description)) = details.normalize() else {
            return Ok(None);
        };
        let _naming = NAMING.lock().await;
        let app_name = unused_app_name(cx, &project.name, &name).await?;
        let service = toasty::create!(Service {
            name,
            app_name,
            description,
            project_id: project.id,
            created_at: now(),
        })
        .exec(&mut db(cx))
        .await?;
        Ok(Some(service))
    }

    /// Replaces the name and description. The app name stays. Returns `false`, changing
    /// nothing, if the details are invalid.
    pub async fn edit(&mut self, cx: &Cx, details: &Details<'_>) -> Result<bool> {
        let Some((name, description)) = details.normalize() else {
            return Ok(false);
        };
        self.update()
            .name(name)
            .description(description)
            .exec(&mut db(cx))
            .await?;
        Ok(true)
    }

    /// Deletes the service.
    pub async fn remove(self, cx: &Cx) -> Result<()> {
        Service::filter_by_id(self.id)
            .delete()
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }

    /// Deletes every service of a project.
    pub async fn remove_all_in(cx: &Cx, project_id: u64) -> Result<()> {
        Service::filter_by_project_id(project_id)
            .delete()
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }
}

/// Returns an app name no other service uses, such as `shop-api`, or `shop-api-2`
/// when that one is taken.
async fn unused_app_name(cx: &Cx, project: &str, service: &str) -> Result<String> {
    let mut base: String = [slug(project), slug(service)]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if base.is_empty() {
        base = "app".to_owned();
    }
    // Leaves room for a suffix.
    base.truncate(MAX_APP_NAME_LEN - 4);
    let base = base.trim_end_matches('-');

    let mut db = db(cx);
    for attempt in 1.. {
        let candidate = match attempt {
            1 => base.to_owned(),
            n => format!("{base}-{n}"),
        };
        let taken = Service::filter_by_app_name(&candidate)
            .first()
            .exec(&mut db)
            .await?
            .is_some();
        if !taken {
            return Ok(candidate);
        }
    }
    unreachable!("some suffix is free")
}

/// Lowercases ASCII letters and digits and joins the runs between them with dashes,
/// so `My API!` becomes `my-api`.
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}
