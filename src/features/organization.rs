//! Organizations, which group a user's projects, stored in the database.

use topcoat::{Result, context::Cx};

use super::user::User;
use crate::db::db;

/// A group of projects owned by one user.
#[derive(Debug, toasty::Model)]
pub struct Organization {
    #[key]
    #[auto]
    pub id: u64,

    pub name: String,

    /// An `http` or `https` image URL shown beside the name.
    pub logo_url: Option<String>,

    #[index]
    pub owner_id: u64,
}

/// The name of the organization created for a user who has none.
const FIRST_ORGANIZATION_NAME: &str = "My Organization";

/// The most characters an organization name may have.
pub const MAX_NAME_LEN: usize = 64;

/// The most characters a logo URL may have.
pub const MAX_LOGO_URL_LEN: usize = 2048;

/// An organization's editable details, as the user typed them.
pub struct OrganizationDetails<'a> {
    pub name: &'a str,
    pub logo_url: &'a str,
}

impl OrganizationDetails<'_> {
    /// Returns the trimmed name and logo URL, or `None` if either is invalid. An empty
    /// logo URL means no logo.
    fn normalize(&self) -> Option<(String, Option<String>)> {
        let name = self.name.trim();
        let logo_url = self.logo_url.trim();
        let valid_name = !name.is_empty() && name.chars().count() <= MAX_NAME_LEN;
        let valid_logo_url = logo_url.is_empty()
            || (logo_url.len() <= MAX_LOGO_URL_LEN
                && (logo_url.starts_with("https://") || logo_url.starts_with("http://")));
        (valid_name && valid_logo_url).then(|| {
            let logo_url = (!logo_url.is_empty()).then(|| logo_url.to_owned());
            (name.to_owned(), logo_url)
        })
    }
}

impl Organization {
    /// Returns the organizations `owner` owns, oldest first.
    pub async fn owned_by(cx: &Cx, owner: &User) -> Result<Vec<Organization>> {
        let organizations = Organization::filter_by_owner_id(owner.id)
            .order_by(Organization::fields().id().asc())
            .exec(&mut db(cx))
            .await?;
        Ok(organizations)
    }

    /// Returns the organization with this ID if `owner` owns it.
    pub async fn find_owned(cx: &Cx, owner: &User, id: u64) -> Result<Option<Organization>> {
        let organization = Organization::filter_by_id(id)
            .first()
            .exec(&mut db(cx))
            .await?;
        Ok(organization.filter(|organization| organization.owner_id == owner.id))
    }

    /// Returns the organization `user` opens after signing in. A user without
    /// organizations gets one, which becomes their default.
    pub async fn default_for(cx: &Cx, user: &mut User) -> Result<Organization> {
        let mut organizations = Self::owned_by(cx, user).await?;
        if organizations.is_empty() {
            let details = OrganizationDetails {
                name: FIRST_ORGANIZATION_NAME,
                logo_url: "",
            };
            let organization = Self::add(cx, user, &details)
                .await?
                .expect("the name is valid");
            user.set_default_organization(cx, organization.id).await?;
            return Ok(organization);
        }

        let default = organizations
            .iter()
            .position(|organization| Some(organization.id) == user.default_organization_id)
            .unwrap_or(0);
        Ok(organizations.swap_remove(default))
    }

    /// Creates an organization owned by `owner`. Returns `None`, creating nothing, if
    /// the details are invalid.
    pub async fn add(
        cx: &Cx,
        owner: &User,
        details: &OrganizationDetails<'_>,
    ) -> Result<Option<Organization>> {
        let Some((name, logo_url)) = details.normalize() else {
            return Ok(None);
        };
        let organization = toasty::create!(Organization {
            name,
            logo_url,
            owner_id: owner.id,
        })
        .exec(&mut db(cx))
        .await?;
        Ok(Some(organization))
    }

    /// Replaces the name and logo. Returns `false`, changing nothing, if the details are
    /// invalid.
    pub async fn edit(&mut self, cx: &Cx, details: &OrganizationDetails<'_>) -> Result<bool> {
        let Some((name, logo_url)) = details.normalize() else {
            return Ok(false);
        };
        self.update()
            .name(name)
            .logo_url(logo_url)
            .exec(&mut db(cx))
            .await?;
        Ok(true)
    }

    /// Deletes the organization. Returns `false`, deleting nothing, if it is the owner's
    /// last one, since a user always has at least one.
    ///
    /// When it was the owner's default, their oldest remaining organization takes over.
    pub async fn remove(self, cx: &Cx, owner: &mut User) -> Result<bool> {
        let remaining: Vec<_> = Self::owned_by(cx, owner)
            .await?
            .into_iter()
            .filter(|organization| organization.id != self.id)
            .collect();
        let Some(oldest) = remaining.first() else {
            return Ok(false);
        };

        if owner.default_organization_id == Some(self.id) {
            owner.set_default_organization(cx, oldest.id).await?;
        }
        Organization::filter_by_id(self.id)
            .delete()
            .exec(&mut db(cx))
            .await?;
        Ok(true)
    }
}
