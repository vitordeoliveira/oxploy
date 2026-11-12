//! Deployments: each time oxploy fetches a service's code, builds it and starts the
//! result. The steps themselves are in [`deploy`](super::deploy).

use topcoat::{Result, context::Cx};

use crate::db::db;

/// One build of a service, and whether it went live.
#[derive(Debug, toasty::Model)]
pub struct Deployment {
    #[key]
    #[auto]
    pub id: u64,

    #[index]
    pub service_id: u64,

    pub status: DeploymentStatus,

    /// The commit that was built, once fetched.
    pub commit: Option<String>,

    /// The first line of the commit's message, or the title of the GitHub release
    /// that was deployed.
    pub commit_message: Option<String>,

    /// The tag of the GitHub release that was deployed, such as `v1.2.0`, for a
    /// service that deploys releases.
    pub tag: Option<String>,

    /// The file name of the built binary, once built.
    pub binary: Option<String>,

    /// The Docker image built, such as `oxploy-shop-api:42`, for a service built with
    /// Docker.
    pub image: Option<String>,

    /// The service's build path when this was built, which the binary runs in. `None`
    /// is the repository root.
    pub build_path: Option<String>,

    /// When the deployment was asked for, in seconds since the Unix epoch.
    pub created_at: i64,

    /// When it succeeded or failed, in seconds since the Unix epoch.
    pub finished_at: Option<i64>,
}

/// How far a deployment got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, toasty::Embed)]
pub enum DeploymentStatus {
    /// Waiting for another build to finish.
    Queued,
    /// Fetching the code and building it.
    Building,
    /// Starting the new binary.
    Starting,
    /// Running; the service's release unless a later one replaced it.
    Succeeded,
    Failed,
}

impl DeploymentStatus {
    /// Whether the deployment is still in progress.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Building | Self::Starting)
    }
}

/// How many deployments of a service the Deployments tab lists.
pub const LISTED: usize = 30;

impl Deployment {
    /// Returns the latest deployments of a service, newest first.
    pub async fn latest_of(cx: &Cx, service_id: u64) -> Result<Vec<Deployment>> {
        let deployments = Deployment::filter_by_service_id(service_id)
            .order_by(Deployment::fields().id().desc())
            .limit(LISTED)
            .exec(&mut db(cx))
            .await?;
        Ok(deployments)
    }

    /// Returns the deployment with this ID if it belongs to the service.
    pub async fn find_of(cx: &Cx, service_id: u64, id: u64) -> Result<Option<Deployment>> {
        let deployment = Deployment::filter_by_id(id).first().exec(&mut db(cx)).await?;
        Ok(deployment.filter(|deployment| deployment.service_id == service_id))
    }
}
