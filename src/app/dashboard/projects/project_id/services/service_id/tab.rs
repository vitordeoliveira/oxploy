//! The service's tabs other than General, one URL segment each.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        StatusCode,
        content::Form,
        error::{not_found, see_other},
        module_param, page, path_param,
    },
    view::{View, view},
};

use super::variables::Draft;
use super::{ServiceTab, current_service, service_view};

module_param!(pub tab: ServiceTab, error = not_found);

#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    let tab = *path_param::<Tab>(cx)?;
    Ok(view! { service_view(tab: tab) })
}

/// The form on the Environment tab.
#[derive(Deserialize)]
struct VariablesForm {
    #[serde(default)]
    variables: String,
}

/// Saves the Environment tab's variables, or shows them again with what is wrong.
#[page(POST)]
async fn save(cx: &Cx, Form(form): Form<VariablesForm>) -> Result<impl View> {
    let tab = *path_param::<Tab>(cx)?;
    if tab != ServiceTab::Environment {
        return Err(not_found().into());
    }
    let (project, mut service) = current_service(cx).await?;
    let problem = match service.set_environment(cx, &form.variables).await? {
        Ok(()) => return Err(see_other(tab.url(cx, &project, &service)).into()),
        Err(problem) => problem.to_string(),
    };
    let draft = Draft {
        text: form.variables,
        problem,
    };

    Ok(view! {
        (StatusCode::UNPROCESSABLE_ENTITY)
        service_view(tab: tab, draft: Some(&draft))
    })
}
