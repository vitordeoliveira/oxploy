//! The Logs tab: what the running app prints, as it prints it.

use std::time::Duration;

use topcoat::{
    Result,
    context::Cx,
    runtime::connected,
    view::{View, component, emit, live, view},
};

use super::{log_lines, settings_card};
use crate::features::app_files::{AppFiles, tail};
use crate::features::service::Service;

/// How many lines of the app's log the tab shows, from its end.
const LOG_LINES: usize = 1000;

/// How often the tab reads the log again.
const INTERVAL: Duration = Duration::from_secs(1);

/// The end of the app's log, refreshed while the page is open.
#[component]
pub async fn logs_tab(cx: &Cx, service: &Service) -> Result<impl View> {
    let files = AppFiles::of(&service.app_name);

    Ok(view! {
        settings_card(
            title: "Logs",
            text: "What the app prints, newest at the bottom. The lines oxploy adds are in bold.",
            (live! {
                loop {
                    let log = tail(&files.app_log(), LOG_LINES);
                    let token = emit! { log_lines(log: &log, empty: "Nothing yet: the app has not run.") }?;
                    if !connected(cx) {
                        break Ok(token);
                    }
                    tokio::time::sleep(INTERVAL).await;
                }
            })
        )
    })
}
