//! The services of a project. Each one has a page of its own.

pub mod service_id;

use topcoat::{
    Result,
    context::Cx,
    router::{
        content::Form,
        error::{SeeOther, bad_request, redirect, see_other},
        href, page, route,
    },
};

use super::{ProjectId, current_project};
use crate::app::dashboard::projects::{DetailsForm, INVALID_DETAILS};
use crate::features::service::Service;
use service_id::ServiceId;

/// The project's page lists its services.
#[page]
async fn page(cx: &Cx) -> Result<()> {
    let project = current_project(cx).await?;
    Err(redirect(href!(super::page, ProjectId(project.id)).resolve(cx)).into())
}

/// Creates a service in the project and opens it.
#[route(POST)]
pub async fn create(cx: &Cx, Form(form): Form<DetailsForm>) -> Result<SeeOther> {
    let project = current_project(cx).await?;
    let Some(service) = Service::add(cx, &project, &form.details()).await? else {
        return Err(bad_request(INVALID_DETAILS).into());
    };
    Ok(see_other(
        href!(service_id::page, ProjectId(project.id), ServiceId(service.id)).resolve(cx),
    ))
}
