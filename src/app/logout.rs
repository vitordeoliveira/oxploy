use topcoat::{
    Result,
    context::Cx,
    router::{
        error::{SeeOther, see_other},
        href, route,
    },
};

use crate::features::session::UserSession;

/// Signs the browser out and sends it to the sign-in page.
#[route(POST)]
pub async fn logout(cx: &Cx) -> Result<SeeOther> {
    UserSession::sign_out(cx).await?;
    Ok(see_other(href!(super::login::page).resolve(cx)))
}
