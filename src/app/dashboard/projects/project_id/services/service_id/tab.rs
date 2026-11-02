//! The service's tabs other than General, one URL segment each.

use topcoat::{
    Result,
    context::Cx,
    router::{module_param, page, path_param},
    view::{View, view},
};

use super::{ServiceTab, service_view};

module_param!(pub tab: ServiceTab, error = not_found);

#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    let tab = *path_param::<Tab>(cx)?;
    Ok(view! { service_view(tab: tab) })
}
