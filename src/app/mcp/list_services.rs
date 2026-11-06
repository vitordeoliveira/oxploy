//! The `list_services` tool: every app on the server, across organizations, with where
//! it stands and what it takes now.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};
use topcoat::context::Cx;

use super::{crashes, mib, moment, round, status_code};
use crate::db::db;
use crate::features::deploy::Deployer;
use crate::features::deployment::Deployment;
use crate::features::domain::Domain;
use crate::features::organization::Organization;
use crate::features::project::Project;
use crate::features::service::Service;

/// The name agents call the tool by.
pub const NAME: &str = "list_services";

/// The statuses an app can be in, as the tools name them.
const STATUSES: [&str; 5] = ["running", "stopped", "restarting", "deploying", "not_deployed"];

/// What the tool does, for the agent deciding whether to call it.
const DESCRIPTION: &str = "Lists every app oxploy runs on this server, across all \
    organizations, sorted by organization, project and name. Each has its app_name, \
    which the other tools take; its status: running, stopped by a user, restarting \
    after it exited on its own, deploying, or not_deployed yet; the port it listens on \
    at 127.0.0.1 and the URLs of its domains; the release that runs, by deployment, \
    commit or GitHub release tag; the CPU and memory it takes now, with how many \
    processes it runs; how often it exited on its own since it was last deployed or \
    started; and its latest deployment. CPU is the busy share of all CPUs together.";

/// The tool's name, description and arguments, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": NAME,
        "title": "List apps",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": STATUSES,
                    "description": "Only the apps with this status.",
                },
            },
            "additionalProperties": false,
        },
        "annotations": { "readOnlyHint": true, "openWorldHint": false },
    })
}

/// The tool's arguments, as the agent sends them. An unknown one is refused, so a
/// misspelled filter is never silently ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    status: Option<String>,
}

/// Lists the apps the arguments ask for. The inner error says what is wrong with the
/// arguments; the outer one is a failure of the database.
pub async fn call(cx: &Cx, arguments: Value) -> topcoat::Result<Result<Value, String>> {
    let arguments: Arguments = match super::arguments(arguments) {
        Ok(arguments) => arguments,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if let Some(status) = &arguments.status
        && !STATUSES.contains(&status.as_str())
    {
        return Ok(Err(format!(
            "There is no status {status}. It is one of: {}.",
            STATUSES.join(", ")
        )));
    }
    let mut db = db(cx);
    let organizations: HashMap<u64, String> = Organization::all()
        .exec(&mut db)
        .await?
        .into_iter()
        .map(|organization| (organization.id, organization.name))
        .collect();
    let projects: HashMap<u64, Project> = Project::all()
        .exec(&mut db)
        .await?
        .into_iter()
        .map(|project| (project.id, project))
        .collect();
    let mut domains: HashMap<u64, Vec<String>> = HashMap::new();
    for domain in Domain::all_in(&db).await? {
        domains.entry(domain.service_id).or_default().push(domain.url());
    }
    let deployer = Deployer::get(cx);

    let mut listed = Vec::new();
    for service in Service::all().exec(&mut db).await? {
        let status = status_code(deployer.status(&service));
        if arguments.status.as_deref().is_some_and(|wanted| wanted != status) {
            continue;
        }
        let project = projects.get(&service.project_id);
        let organization = project.and_then(|project| organizations.get(&project.organization_id));
        let release = match service.release_id {
            Some(id) => Deployment::filter_by_id(id).first().exec(&mut db).await?,
            None => None,
        };
        let latest = Deployment::filter_by_service_id(service.id)
            .order_by(Deployment::fields().id().desc())
            .first()
            .exec(&mut db)
            .await?;
        let now = deployer
            .usage()
            .app(service.id)
            .last()
            .and_then(|sample| Some((sample.process?, sample.load)))
            .map(|(process, load)| {
                json!({
                    "cpu_percent": round(f64::from(load.cpu)),
                    "memory_mib": mib(load.memory as f64),
                    "processes": process.count,
                })
            });
        let sort_key = (
            organization.cloned().unwrap_or_default(),
            project.map(|project| project.name.clone()).unwrap_or_default(),
            service.name.clone(),
        );
        listed.push((
            sort_key,
            json!({
                "app_name": service.app_name,
                "name": service.name,
                "organization": organization,
                "project": project.map(|project| &project.name),
                "status": status,
                "port": service.port,
                "domains": domains.remove(&service.id).unwrap_or_default(),
                "release": release.map(|release| json!({
                    "deployment_id": release.id,
                    "commit": release.commit,
                    "tag": release.tag,
                    "message": release.commit_message,
                    "deployed_at": release.finished_at.and_then(moment),
                })),
                "now": now,
                "crashes": crashes(deployer, service.id),
                "latest_deployment": latest.map(|latest| json!({
                    "id": latest.id,
                    "status": super::list_deployments::status_code(latest.status),
                    "created_at": moment(latest.created_at),
                    "finished_at": latest.finished_at.and_then(moment),
                })),
            }),
        ));
    }
    listed.sort_by(|(a, _), (b, _)| a.cmp(b));
    let services: Vec<_> = listed.into_iter().map(|(_, service)| service).collect();
    Ok(Ok(json!({ "count": services.len(), "services": services })))
}
