//! The MCP server at `/mcp`, through which AI agents, such as a bot or Claude Code, reach
//! what their token opens: with a server token, the machine, oxploy itself, and the
//! usage, logs and deployments of every app it runs; with an organization's, the tools
//! of its apps that were turned on, which [`apps`] passes on to each app's own server.
//!
//! It speaks the Model Context Protocol over Streamable HTTP in its simplest form: each
//! POST carries one JSON-RPC message and gets plain JSON back, with no session and no
//! event stream, which is all a server with only tools needs. A client sends one of the
//! tokens the super admin makes on the MCP page as `Authorization: Bearer <token>`. The
//! router already turns away requests sent from other sites' pages, as the protocol asks
//! of a server.
//!
//! Each server tool has a file of its own beside this one, and a line each in
//! [`definitions`] and [`call_tool`].

mod app_logs;
mod apps;
mod deployment_log;
mod list_deployments;
mod list_services;
mod server_status;
mod service_usage;

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use topcoat::{
    Result,
    context::Cx,
    router::{
        HeaderValue, StatusCode,
        content::Json,
        header::WWW_AUTHENTICATE,
        request::Bytes,
        response::{IntoResponse, Response},
        route,
    },
};

use crate::db::db;
use crate::features::app_files::tail;
use crate::features::deploy::{AppStatus, Deployer};
use crate::features::mcp::{self, McpToken, Reach};
use crate::features::organization::Organization;
use crate::features::runtime::Exits;
use crate::features::service::Service;
use crate::features::time_zone;
use crate::features::update::VERSION;

/// The protocol versions the server speaks, newest first. The few methods it answers
/// read the same in all of them.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

/// How long a request without a valid token waits before the answer, to slow down
/// guessing.
const FAILED_TOKEN_DELAY: Duration = Duration::from_secs(1);

/// JSON-RPC's error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// What the server tells an agent with a server token about itself when it connects.
const INSTRUCTIONS: &str = "oxploy deploys Rust apps on this server and runs each as a \
    process of its own. This MCP server reads the machine, oxploy itself, and the usage, \
    logs and deployments of every app, across all organizations. It only reads: changes \
    are made in the oxploy panel. The tools that take one app name it by its app_name, \
    as list_services returns it. Usage history covers only the last 10 minutes, kept in \
    memory since oxploy started. Times are in the panel's time zone.";

/// What a token reaches.
enum Scope {
    /// The machine, oxploy itself, and every app it runs.
    Server,
    /// The tools of one organization's apps that were turned on, all of them or only
    /// the ones picked for the token.
    Organization { id: u64, name: String, reach: Reach },
}

impl Scope {
    /// What `token` reaches, or `None` if its organization is gone.
    async fn of(cx: &Cx, token: &McpToken) -> Result<Option<Scope>> {
        let Some(id) = token.organization_id else {
            return Ok(Some(Scope::Server));
        };
        let Some(organization) = Organization::filter_by_id(id).first().exec(&mut db(cx)).await? else {
            return Ok(None);
        };
        Ok(Some(Scope::Organization {
            id,
            name: organization.name,
            reach: token.reach(cx).await?,
        }))
    }
}

/// Receives one MCP message and answers it.
#[route(POST)]
pub async fn receive(cx: &Cx, body: Bytes) -> Result<Response> {
    let scope = match mcp::authorized(cx).await? {
        Some(token) => Scope::of(cx, &token).await?,
        None => None,
    };
    let Some(scope) = scope else {
        tokio::time::sleep(FAILED_TOKEN_DELAY).await;
        return (
            StatusCode::UNAUTHORIZED,
            [(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))],
            "Unauthorized",
        )
            .into_response(cx);
    };
    let answer = match serde_json::from_slice(&body) {
        Ok(message) => answer(cx, &scope, &message).await,
        Err(_) => Some(failure(Value::Null, PARSE_ERROR, "The body is not JSON.")),
    };
    match answer {
        Some(answer) => Json(answer).into_response(cx),
        // A notification gets no answer, only the acknowledgement the protocol asks for.
        None => StatusCode::ACCEPTED.into_response(cx),
    }
}

/// A JSON-RPC error: its code and what went wrong.
#[derive(Debug)]
struct Failure {
    code: i64,
    message: String,
}

impl Failure {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The answer to one JSON-RPC message, or `None` for a notification, such as
/// `notifications/initialized`, which gets none.
async fn answer(cx: &Cx, scope: &Scope, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        let id = id.unwrap_or(Value::Null);
        return Some(failure(id, INVALID_REQUEST, "Expected one JSON-RPC request."));
    };
    let id = id?;
    let params = message.get("params").unwrap_or(&Value::Null);
    let outcome = match (method, scope) {
        ("initialize", scope) => Ok(initialize(params, scope)),
        ("ping", _) => Ok(json!({})),
        ("tools/list", Scope::Server) => Ok(json!({ "tools": definitions() })),
        ("tools/list", Scope::Organization { id, reach, .. }) => match apps::definitions(cx, *id, reach).await {
            Ok(tools) => Ok(json!({ "tools": tools })),
            Err(error) => Err(failed("tools/list", &error)),
        },
        ("tools/call", Scope::Server) => call_tool(cx, params).await,
        ("tools/call", Scope::Organization { id, reach, .. }) => match apps::call(cx, *id, reach, params).await {
            Ok(outcome) => outcome,
            Err(error) => Err(failed("tools/call", &error)),
        },
        _ => Err(Failure::new(METHOD_NOT_FOUND, format!("Unknown method: {method}."))),
    };
    Some(match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => failure(id, error.code, &error.message),
    })
}

/// The answer to `initialize`: the protocol version, the client's when the server
/// speaks it or else the newest, and what the server offers to `scope`.
fn initialize(params: &Value, scope: &Scope) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested
        .filter(|version| PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    let instructions = match scope {
        Scope::Server => INSTRUCTIONS.to_owned(),
        Scope::Organization { name, .. } => format!(
            "oxploy deploys Rust apps on this server. This MCP server passes on the tools of \
             the apps of the organization {name}, which each app's own MCP server offers. \
             Only the tools turned on in oxploy for this token are listed. Each is named \
             after its app, as <app>__<tool>, and its description starts with the app it \
             comes from. A tool of an app that is not running is not listed."
        ),
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "oxploy",
            "title": "oxploy",
            "version": VERSION,
        },
        "instructions": instructions,
    })
}

/// A database failure while answering `method`, logged, as the client sees it.
fn failed(method: &str, error: &topcoat::Error) -> Failure {
    eprintln!("oxploy: the MCP request {method} failed: {error}");
    Failure::new(INTERNAL_ERROR, "The request failed. Try again later.")
}

/// Every tool's name, description and arguments, for `tools/list`.
fn definitions() -> Vec<Value> {
    vec![
        server_status::definition(),
        list_services::definition(),
        service_usage::definition(),
        app_logs::definition(),
        list_deployments::definition(),
        deployment_log::definition(),
    ]
}

/// Runs the tool `params` names. Arguments the tool refuses come back as the tool's own
/// error, which the agent reads and can correct; an unknown tool or a database failure
/// is an error of the protocol.
async fn call_tool(cx: &Cx, params: &Value) -> Result<Value, Failure> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let outcome = match name {
        server_status::NAME => server_status::call(cx, arguments).await,
        list_services::NAME => list_services::call(cx, arguments).await,
        service_usage::NAME => service_usage::call(cx, arguments).await,
        app_logs::NAME => app_logs::call(cx, arguments).await,
        list_deployments::NAME => list_deployments::call(cx, arguments).await,
        deployment_log::NAME => deployment_log::call(cx, arguments).await,
        _ => return Err(Failure::new(INVALID_PARAMS, format!("Unknown tool: {name}."))),
    };
    match outcome {
        Ok(Ok(data)) => Ok(tool_result(data.to_string(), false)),
        Ok(Err(refusal)) => Ok(tool_result(refusal, true)),
        Err(error) => {
            eprintln!("oxploy: the MCP tool {name} failed: {error}");
            Err(Failure::new(INTERNAL_ERROR, "The tool failed. Try again later."))
        }
    }
}

/// A tool's result: its text, and whether it is an error.
fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// A JSON-RPC error response.
fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool's arguments, or what is wrong with them, for the agent to correct.
fn arguments<T: DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|error| format!("The arguments are wrong: {error}."))
}

/// The service with this app name, or a refusal that says how to find one.
async fn service_named(cx: &Cx, app_name: &str) -> Result<Result<Service, String>> {
    let service = Service::filter_by_app_name(app_name)
        .first()
        .exec(&mut db(cx))
        .await?;
    Ok(service.ok_or_else(|| {
        format!("There is no app named {app_name}. list_services lists each app's app_name.")
    }))
}

/// The last `count` lines of a log, keeping only those with `contains` in them, ignoring
/// case, when it is given. Only the end of a long log is read; see [`tail`].
fn lines_of(path: &Path, count: usize, contains: Option<&str>) -> String {
    let Some(contains) = contains else {
        return tail(path, count);
    };
    let wanted = contains.to_lowercase();
    let read = tail(path, usize::MAX);
    let matching: Vec<_> = read
        .lines()
        .filter(|line| line.to_lowercase().contains(&wanted))
        .collect();
    matching[matching.len().saturating_sub(count)..].join("\n")
}

/// A service's status, as the tools name it.
fn status_code(status: AppStatus) -> &'static str {
    match status {
        AppStatus::NotDeployed => "not_deployed",
        AppStatus::Deploying => "deploying",
        AppStatus::Running => "running",
        AppStatus::Restarting => "restarting",
        AppStatus::Stopped => "stopped",
    }
}

/// How often the app exited on its own since a deployment or the user last started it,
/// as this run of the panel saw it, and when and how it last did.
fn crashes(deployer: &Deployer, service_id: u64) -> Value {
    let Exits { count, last } = deployer.exits(service_id);
    let (at, how) = match last {
        Some((at, how)) => (Some(system_moment(at)), Some(how)),
        None => (None, None),
    };
    json!({ "count": count, "last_at": at, "last_how": how })
}

/// A moment in seconds since the Unix epoch, in the panel's time zone, such as
/// `2026-11-06T17:04:47-03:00`.
fn moment(seconds: i64) -> Option<String> {
    let timestamp = jiff::Timestamp::from_second(seconds).ok()?;
    Some(time_zone::at(timestamp).strftime("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

/// A moment of the system clock, in the panel's time zone.
fn system_moment(at: SystemTime) -> String {
    jiff::Timestamp::try_from(at)
        .map(|timestamp| time_zone::at(timestamp).strftime("%Y-%m-%dT%H:%M:%S%:z").to_string())
        .unwrap_or_default()
}

/// Seconds since a moment of the system clock.
fn seconds_since(at: SystemTime) -> u64 {
    SystemTime::now().duration_since(at).map_or(0, |elapsed| elapsed.as_secs())
}

/// A span of seconds in its two largest units, such as `3 days 4 hours`.
fn duration(seconds: u64) -> String {
    const UNITS: [(u64, &str); 4] = [(86_400, "day"), (3_600, "hour"), (60, "minute"), (1, "second")];
    let parts: Vec<_> = UNITS
        .iter()
        .scan(seconds, |left, &(size, unit)| {
            let count = *left / size;
            *left %= size;
            Some((count, unit))
        })
        .skip_while(|(count, _)| *count == 0)
        .take(2)
        .filter(|(count, _)| *count > 0)
        .map(|(count, unit)| format!("{count} {unit}{}", if count == 1 { "" } else { "s" }))
        .collect();
    if parts.is_empty() {
        "0 seconds".to_owned()
    } else {
        parts.join(" ")
    }
}

/// Bytes in mebibytes, to one decimal.
fn mib(bytes: f64) -> f64 {
    round(bytes / (1024.0 * 1024.0))
}

/// A number to one decimal, which is as precise as the readings are.
fn round(number: f64) -> f64 {
    (number * 10.0).round() / 10.0
}

/// `part` as a percentage of `whole`, to one decimal.
fn percent(part: u64, whole: u64) -> f64 {
    match whole {
        0 => 0.0,
        whole => round(part as f64 * 100.0 / whole as f64),
    }
}

#[cfg(test)]
mod tests {
    use topcoat::context::CxTestBuilder;

    use super::*;

    async fn send(message: Value) -> Option<Value> {
        answer(&CxTestBuilder::new().build(), &Scope::Server, &message).await
    }

    #[tokio::test]
    async fn speaks_the_clients_version_or_its_newest() {
        let answer = send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" },
        }))
        .await
        .unwrap();
        assert_eq!(answer["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(answer["id"], 1);

        let answer = send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "initialize",
            "params": { "protocolVersion": "2030-01-01" },
        }))
        .await
        .unwrap();
        assert_eq!(answer["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[tokio::test]
    async fn an_organization_token_reaches_its_apps_and_not_the_server() {
        let cx = CxTestBuilder::new().build();
        let scope = Scope::Organization { id: 7, name: "Porta Global".to_owned(), reach: Reach::All };

        let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        let initialized = answer(&cx, &scope, &message).await.unwrap();
        let instructions = initialized["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("the organization Porta Global"), "{instructions}");

        // The server's tools are not the organization's, and a name without an app is no
        // app's tool.
        let message = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": server_status::NAME },
        });
        let call = answer(&cx, &scope, &message).await.unwrap();
        assert_eq!(call["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn answers_no_notification() {
        let message = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(send(message).await, None);
    }

    #[tokio::test]
    async fn lists_the_tools_as_read_only() {
        let answer = send(json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/list" }))
            .await
            .unwrap();
        assert_eq!(answer["id"], "a");
        let tools = answer["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                server_status::NAME,
                list_services::NAME,
                service_usage::NAME,
                app_logs::NAME,
                list_deployments::NAME,
                deployment_log::NAME,
            ]
        );
        for tool in tools {
            assert_eq!(tool["annotations"]["readOnlyHint"], true, "{}", tool["name"]);
            assert_eq!(tool["inputSchema"]["type"], "object", "{}", tool["name"]);
        }
    }

    #[tokio::test]
    async fn refuses_what_it_does_not_know() {
        let unknown_method = send(json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" }))
            .await
            .unwrap();
        assert_eq!(unknown_method["error"]["code"], METHOD_NOT_FOUND);

        let unknown_tool = send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "deploy" },
        }))
        .await
        .unwrap();
        assert_eq!(unknown_tool["error"]["code"], INVALID_PARAMS);

        let batch = send(json!([{ "jsonrpc": "2.0", "id": 3, "method": "ping" }]))
            .await
            .unwrap();
        assert_eq!(batch["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn reads_the_end_of_a_log_or_the_lines_with_a_text() {
        let dir = std::env::temp_dir().join(format!("oxploy-mcp-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, "started\nGET / 200\nERROR db locked\nGET /a 200\nerror: timeout\n").unwrap();

        assert_eq!(lines_of(&log, 2, None), "GET /a 200\nerror: timeout");
        assert_eq!(lines_of(&log, 10, Some("error")), "ERROR db locked\nerror: timeout");
        assert_eq!(lines_of(&log, 1, Some("ERROR")), "error: timeout");
        assert_eq!(lines_of(&log, 10, Some("panic")), "");
        assert_eq!(lines_of(&dir.join("missing.log"), 10, None), "");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn says_spans_in_their_two_largest_units() {
        assert_eq!(duration(0), "0 seconds");
        assert_eq!(duration(1), "1 second");
        assert_eq!(duration(3_660), "1 hour 1 minute");
        assert_eq!(duration(3 * 86_400 + 4 * 3_600 + 59), "3 days 4 hours");
        assert_eq!(duration(86_400 + 30), "1 day");
    }

    #[test]
    fn rounds_readings_to_one_decimal() {
        assert_eq!(mib(1_572_864.0), 1.5);
        assert_eq!(percent(1, 3), 33.3);
        assert_eq!(percent(5, 0), 0.0);
    }
}
