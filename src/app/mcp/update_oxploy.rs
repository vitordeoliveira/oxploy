//! The `update_oxploy` tool: installs oxploy's newest release, as the panel's Update
//! button does.

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use crate::features::deploy::Deployer;
use crate::features::update::{Blocker, Updater, VERSION};

/// The name agents call the tool by.
pub const NAME: &str = "update_oxploy";

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "Installs oxploy's newest release, as the panel's Update \
    button does. It asks GitHub for the newest release now and, when it is newer than \
    the running version, starts installing it and answers at once: oxploy downloads it, \
    checks it, waits for the deployments in progress, and restarts on it in the same \
    process. The apps keep running. This server answers again within seconds of the \
    restart; server_status then tells the version, or how far the installation got and \
    why it failed.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "Update oxploy",
        "description": DESCRIPTION,
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": true,
            "openWorldHint": true,
        },
    })
}

/// The tool takes no arguments, and refuses any.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {}

/// Starts installing the newest release, if there is one to install. The inner error
/// says why it cannot.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    if let Err(refusal) = super::arguments::<Arguments>(arguments) {
        return Ok(Err(refusal));
    }
    let updater = Updater::get(cx);
    if let Some(installation) = updater.installation().filter(|installation| installation.is_running()) {
        return Ok(Ok(json!({
            "installing": installation.version,
            "running_version": VERSION,
            "step": super::server_status::step_code(&installation.step),
        })));
    }
    updater.check_now().await;
    if let Some(error) = updater.check().error {
        return Ok(Err(format!("GitHub could not be asked for oxploy's releases: {error}")));
    }
    let Some(release) = updater.available() else {
        return Ok(Ok(json!({ "up_to_date": true, "running_version": VERSION })));
    };
    if let Some(blocker) = updater.blocker(&release) {
        return Ok(Err(format!(
            "oxploy {} is out, but this panel cannot install it on its own: {}",
            release.version,
            blocked(&blocker)
        )));
    }
    if !updater.install(&release, Deployer::get(cx).clone()) {
        return Ok(Err("oxploy could not start installing the update. Try again later.".to_owned()));
    }
    Ok(Ok(json!({
        "installing": release.version,
        "running_version": VERSION,
        "step": "downloading",
    })))
}

/// Why the panel cannot install an update on its own, for the agent.
fn blocked(blocker: &Blocker) -> String {
    match blocker {
        Blocker::DevelopmentBuild => "it is a development build, which updates from its code.".to_owned(),
        Blocker::NoBuild(machine) => format!("the release has no build for this machine, {machine}."),
        Blocker::ReadOnly(folder) => format!(
            "its user cannot change the folder it is installed in, {}.",
            folder.display()
        ),
        Blocker::NoBinary => "it could not find its own binary.".to_owned(),
    }
}
