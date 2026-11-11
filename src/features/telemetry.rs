//! Usage reporting: once a day, oxploy tells oxploy.com that it runs, with its version,
//! the machine it runs on and a random ID it made the first time it started, which tells
//! its reports apart and says nothing about the server or its owner. When the super admin
//! shares it in Settings, the report also says what the server runs, in numbers only.
//!
//! The super admin turns it off in Settings, and `OXPLOY_TELEMETRY=off` or `DO_NOT_TRACK=1`
//! turn it off whatever Settings says. A development build does not report, unless
//! `OXPLOY_TELEMETRY_URL` sends its reports somewhere, such as a site running locally.

use std::time::Duration;

use serde_json::{Value, json};
use toasty::Db;

use super::databases::ManagedDatabase;
use super::deployment::{Deployment, DeploymentStatus};
use super::domain::Domain;
use super::mcp::McpToken;
use super::organization::Organization;
use super::project::now;
use super::service::{BuildKind, Service};
use super::update::{VERSION, machine};
use super::user::User;

/// Where the reports go, unless `OXPLOY_TELEMETRY_URL` says otherwise.
const ENDPOINT: &str = "https://oxploy.com/api/ping";

/// How long oxploy waits after it starts before it reports, and how often it then looks
/// whether a report is due.
const FIRST_LOOK: Duration = Duration::from_secs(60);
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);

/// How long after a report the next one is due. A little under a day, so looking every
/// hour does not push the reports later each day.
const DUE_AFTER: i64 = 23 * 60 * 60;

/// How long a report may take to send.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The longest reason for a failed report that is kept.
const MAX_ERROR_LEN: usize = 300;

/// The server's usage reporting. There is one, made the first time oxploy starts.
#[derive(Debug, toasty::Model)]
pub struct Telemetry {
    #[key]
    #[auto]
    pub id: u64,

    /// The random ID the reports carry: 32 hexadecimal digits.
    pub install_id: String,

    /// Whether oxploy reports that it runs. On until the super admin turns it off.
    pub reports: bool,

    /// Whether the reports say what the server runs, in numbers. Off until the super
    /// admin turns it on.
    pub shares_usage: bool,

    /// When the last report went out, in seconds since the Unix epoch.
    pub sent_at: Option<i64>,

    /// When the last report failed, and why, if it failed after the last one that went
    /// out.
    pub failed_at: Option<i64>,
    pub error: Option<String>,

    /// When it was made, in seconds since the Unix epoch.
    pub created_at: i64,
}

/// Why reporting is off whatever Settings says: the variable that turns it off, or a
/// development build.
pub fn turned_off() -> Option<&'static str> {
    let set = |name: &str| std::env::var(name).unwrap_or_default().trim().to_ascii_lowercase();
    if matches!(set("OXPLOY_TELEMETRY").as_str(), "off" | "0" | "false" | "no") {
        return Some("OXPLOY_TELEMETRY=off");
    }
    if matches!(set("DO_NOT_TRACK").as_str(), "1" | "true" | "yes") {
        return Some("DO_NOT_TRACK=1");
    }
    if cfg!(debug_assertions) && std::env::var_os("OXPLOY_TELEMETRY_URL").is_none() {
        return Some("a development build");
    }
    None
}

/// Where the reports go.
fn endpoint() -> String {
    std::env::var("OXPLOY_TELEMETRY_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| ENDPOINT.to_owned())
}

impl Telemetry {
    /// The server's usage reporting, made the first time with a new random ID.
    pub async fn get_in(db: &Db) -> toasty::Result<Telemetry> {
        let mut db = db.clone();
        if let Some(telemetry) = Telemetry::all().first().exec(&mut db).await? {
            return Ok(telemetry);
        }
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).expect("the system provides random bytes");
        toasty::create!(Telemetry {
            install_id: bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            reports: true,
            shares_usage: false,
            sent_at: None,
            failed_at: None,
            error: None,
            created_at: now(),
        })
        .exec(&mut db)
        .await
    }

    /// Starts reporting, a minute after oxploy starts and then once a day, on a task of
    /// its own.
    pub fn start(db: Db) {
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_LOOK).await;
            loop {
                if let Err(error) = Telemetry::report_if_due(&db).await {
                    eprintln!("oxploy: the usage report could not be prepared: {error}");
                }
                tokio::time::sleep(LOOK_EVERY).await;
            }
        });
    }

    /// Sends the report, if reporting is on and the last one went out a day ago.
    async fn report_if_due(db: &Db) -> toasty::Result<()> {
        if turned_off().is_some() {
            return Ok(());
        }
        let telemetry = Telemetry::get_in(db).await?;
        if !telemetry.reports || telemetry.sent_at.is_some_and(|at| now() - at < DUE_AFTER) {
            return Ok(());
        }
        let report = telemetry.report(db).await?;
        let url = endpoint();
        let sent = tokio::task::spawn_blocking(move || send(&url, &report))
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
        let mut db = db.clone();
        let mut telemetry = telemetry;
        match sent {
            Ok(()) => {
                telemetry
                    .update()
                    .sent_at(Some(now()))
                    .failed_at(None)
                    .error(None)
                    .exec(&mut db)
                    .await
            }
            Err(error) => {
                let error: String = error.chars().take(MAX_ERROR_LEN).collect();
                telemetry
                    .update()
                    .failed_at(Some(now()))
                    .error(Some(error))
                    .exec(&mut db)
                    .await
            }
        }
    }

    /// The report as oxploy sends it now.
    pub async fn report(&self, db: &Db) -> toasty::Result<Value> {
        let mut report = json!({
            "install_id": self.install_id,
            "version": VERSION,
            "platform": machine(),
        });
        if self.shares_usage {
            report["usage"] = usage(db).await?;
        }
        Ok(report)
    }

    /// Turns reporting on or off.
    pub async fn set_reports(db: &Db, on: bool) -> toasty::Result<()> {
        let mut telemetry = Telemetry::get_in(db).await?;
        telemetry.update().reports(on).exec(&mut db.clone()).await
    }

    /// Turns sharing what the server runs on or off.
    pub async fn set_shares_usage(db: &Db, on: bool) -> toasty::Result<()> {
        let mut telemetry = Telemetry::get_in(db).await?;
        telemetry.update().shares_usage(on).exec(&mut db.clone()).await
    }
}

/// What the server runs, in numbers: no names, domains, repositories or anything else
/// that tells whose it is.
async fn usage(db: &Db) -> toasty::Result<Value> {
    let mut db = db.clone();
    let services = Service::all().exec(&mut db).await?;
    let (external, apps): (Vec<&Service>, Vec<&Service>) = services.iter().partition(|service| service.is_external());
    let built = |kind: BuildKind| {
        apps.iter()
            .filter(|service| service.build_kind.unwrap_or(BuildKind::Cargo) == kind)
            .count()
    };
    let week_ago = now() - 7 * 24 * 60 * 60;
    let deployments = Deployment::filter(Deployment::fields().created_at().ge(week_ago))
        .exec(&mut db)
        .await?;
    let failed = deployments
        .iter()
        .filter(|deployment| deployment.status == DeploymentStatus::Failed)
        .count();
    Ok(json!({
        "users": User::all().exec(&mut db).await?.len(),
        "organizations": Organization::all().exec(&mut db).await?.len(),
        "apps": apps.len(),
        "apps_built_with_cargo": built(BuildKind::Cargo),
        "apps_built_with_script": built(BuildKind::Script),
        "apps_deploying_releases": built(BuildKind::Release),
        "apps_with_mcp": apps.iter().filter(|service| service.serves_mcp()).count(),
        "external_mcp_servers": external.len(),
        "mcp_tokens": McpToken::all().exec(&mut db).await?.len(),
        "databases": ManagedDatabase::all().exec(&mut db).await?.len(),
        "domains": Domain::all().exec(&mut db).await?.len(),
        "deployments_7d": deployments.len(),
        "failed_deployments_7d": failed,
    }))
}

/// Sends `report` to `url`, and returns why it did not arrive, if it did not.
fn send(url: &str, report: &Value) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .user_agent(format!("oxploy/{VERSION}"))
        .build()
        .into();
    let response = agent
        .post(url)
        .header("content-type", "application/json")
        .send(report.to_string())
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("{url} answered {status}"))
    }
}

#[cfg(test)]
mod tests {
    use topcoat::context::CxTestBuilder;

    use super::*;
    use crate::db::TestDb;
    use crate::features::project::{Details, Project};

    #[tokio::test]
    async fn a_server_is_known_by_one_random_id() {
        let test = TestDb::new().await;
        let telemetry = Telemetry::get_in(&test.db).await.unwrap();
        assert_eq!(telemetry.install_id.len(), 32);
        assert!(telemetry.install_id.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        assert!(telemetry.reports, "reporting is on until the super admin turns it off");
        assert!(!telemetry.shares_usage, "sharing what it runs waits for a yes");
        assert_eq!(Telemetry::get_in(&test.db).await.unwrap().install_id, telemetry.install_id);
        let other = TestDb::new().await;
        assert_ne!(Telemetry::get_in(&other.db).await.unwrap().install_id, telemetry.install_id);
    }

    #[tokio::test]
    async fn the_report_says_what_it_runs_only_when_shared() {
        let test = TestDb::new().await;
        let cx = CxTestBuilder::new().app_context(test.db.clone()).build();
        let telemetry = Telemetry::get_in(&test.db).await.unwrap();
        let report = telemetry.report(&test.db).await.unwrap();
        let keys: Vec<&str> = report.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["install_id", "platform", "version"]);
        assert_eq!(report["version"], VERSION);

        let project = Project::add(&cx, 7, &Details { name: "Shop", description: "" })
            .await
            .unwrap()
            .unwrap();
        Service::add(&cx, &project, &Details { name: "Shop", description: "" })
            .await
            .unwrap()
            .unwrap();
        Telemetry::set_shares_usage(&test.db, true).await.unwrap();
        let telemetry = Telemetry::get_in(&test.db).await.unwrap();
        let report = telemetry.report(&test.db).await.unwrap();
        let usage = &report["usage"];
        assert_eq!(usage["apps"], 1);
        assert_eq!(usage["apps_built_with_cargo"], 1);
        assert_eq!(usage["external_mcp_servers"], 0);
        assert_eq!(usage["deployments_7d"], 0);
        // Numbers only: nothing that names the server's apps.
        assert!(!report.to_string().to_lowercase().contains("shop"));
        assert!(usage.as_object().unwrap().values().all(Value::is_u64));
    }

    #[tokio::test]
    async fn turning_reporting_off_is_kept() {
        let test = TestDb::new().await;
        Telemetry::set_reports(&test.db, false).await.unwrap();
        assert!(!Telemetry::get_in(&test.db).await.unwrap().reports);
        Telemetry::set_reports(&test.db, true).await.unwrap();
        assert!(Telemetry::get_in(&test.db).await.unwrap().reports);
    }
}
