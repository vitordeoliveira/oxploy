//! The services of a project. Each one has a page of its own.

pub mod service_id;

use serde::Deserialize;
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
use crate::app::dashboard::projects::INVALID_DETAILS;
use crate::features::project::Details;
use crate::features::service::Service;
use service_id::ServiceId;

/// The project's page lists its services.
#[page]
async fn page(cx: &Cx) -> Result<()> {
    let project = current_project(cx).await?;
    Err(redirect(href!(super::page, ProjectId(project.id)).resolve(cx)).into())
}

/// The form that makes a service: an app, or an external MCP server with the URL it
/// answers at and the token it takes.
#[derive(Deserialize)]
pub struct NewServiceForm {
    name: String,
    #[serde(default)]
    description: String,
    /// `external_mcp` for an external MCP server. Anything else makes an app.
    #[serde(default)]
    kind: String,
    #[serde(default)]
    url: String,
    /// The token an external MCP server takes, or empty for oxploy to make one.
    #[serde(default)]
    token: String,
}

/// Creates a service in the project and opens it.
#[route(POST)]
pub async fn create(cx: &Cx, Form(form): Form<NewServiceForm>) -> Result<SeeOther> {
    let project = current_project(cx).await?;
    let details = Details {
        name: &form.name,
        description: &form.description,
    };
    let service = if form.kind == "external_mcp" {
        match Service::add_external(cx, &project, &details, &form.url, Some(&form.token)).await? {
            Ok(service) => service,
            Err(problem) => return Err(bad_request(problem.to_string()).into()),
        }
    } else {
        let Some(service) = Service::add(cx, &project, &details).await? else {
            return Err(bad_request(INVALID_DETAILS).into());
        };
        service
    };
    Ok(see_other(
        href!(service_id::page, ProjectId(project.id), ServiceId(service.id)).resolve(cx),
    ))
}
