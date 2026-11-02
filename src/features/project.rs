//! Projects, which group the services an organization deploys, stored in the database.

use std::time::{SystemTime, UNIX_EPOCH};

use topcoat::{Result, context::Cx};

use super::service::Service;
use crate::db::db;

/// A group of services that are deployed together, such as an app and its workers.
#[derive(Debug, toasty::Model)]
pub struct Project {
    #[key]
    #[auto]
    pub id: u64,

    pub name: String,

    pub description: Option<String>,

    #[index]
    pub organization_id: u64,

    /// When the project was created, in seconds since the Unix epoch.
    pub created_at: i64,
}

/// The most characters the name of a project or service may have.
pub const MAX_NAME_LEN: usize = 64;

/// The most characters the description of a project or service may have.
pub const MAX_DESCRIPTION_LEN: usize = 500;

/// The name and description of a project or service, as the user typed them.
pub struct Details<'a> {
    pub name: &'a str,
    pub description: &'a str,
}

impl Details<'_> {
    /// Returns the trimmed name and description, or `None` if either is invalid. An
    /// empty description means none.
    pub(super) fn normalize(&self) -> Option<(String, Option<String>)> {
        let name = self.name.trim();
        let description = self.description.trim();
        let valid = !name.is_empty()
            && name.chars().count() <= MAX_NAME_LEN
            && description.chars().count() <= MAX_DESCRIPTION_LEN;
        valid.then(|| {
            let description = (!description.is_empty()).then(|| description.to_owned());
            (name.to_owned(), description)
        })
    }
}

impl Project {
    /// Returns the projects of an organization, newest first.
    pub async fn in_organization(cx: &Cx, organization_id: u64) -> Result<Vec<Project>> {
        let projects = Project::filter_by_organization_id(organization_id)
            .order_by(Project::fields().id().desc())
            .exec(&mut db(cx))
            .await?;
        Ok(projects)
    }

    /// Returns the project with this ID if it belongs to the organization.
    pub async fn find_in(cx: &Cx, organization_id: u64, id: u64) -> Result<Option<Project>> {
        let project = Project::filter_by_id(id).first().exec(&mut db(cx)).await?;
        Ok(project.filter(|project| project.organization_id == organization_id))
    }

    /// Creates a project in the organization. Returns `None`, creating nothing, if the
    /// details are invalid.
    pub async fn add(
        cx: &Cx,
        organization_id: u64,
        details: &Details<'_>,
    ) -> Result<Option<Project>> {
        let Some((name, description)) = details.normalize() else {
            return Ok(None);
        };
        let project = toasty::create!(Project {
            name,
            description,
            organization_id,
            created_at: now(),
        })
        .exec(&mut db(cx))
        .await?;
        Ok(Some(project))
    }

    /// Replaces the name and description. Returns `false`, changing nothing, if the
    /// details are invalid.
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

    /// Deletes the project and its services.
    pub async fn remove(self, cx: &Cx) -> Result<()> {
        Service::remove_all_in(cx, self.id).await?;
        Project::filter_by_id(self.id)
            .delete()
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }

    /// Deletes every project of an organization, with their services.
    pub async fn remove_all_in(cx: &Cx, organization_id: u64) -> Result<()> {
        for project in Self::in_organization(cx, organization_id).await? {
            project.remove(cx).await?;
        }
        Ok(())
    }
}

/// Returns the current time in seconds since the Unix epoch.
pub(super) fn now() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    i64::try_from(seconds).unwrap_or(i64::MAX)
}
