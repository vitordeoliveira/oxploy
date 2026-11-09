//! The MCP servers of the apps: where an app serves one, and which of its tools the
//! user turned on.
//!
//! An app serves MCP over Streamable HTTP on its own port, at a path such as `/mcp`.
//! oxploy reaches it at 127.0.0.1 and sends the token it gives the app in
//! `OXPLOY_MCP_TOKEN` as `Authorization: Bearer <token>`. Caddy answers 404 at that path
//! on the app's domains, so only oxploy reaches it.
//!
//! An external MCP server runs elsewhere, as a service of its own kind, and oxploy calls
//! it at its URL with the token it takes. Those calls go only over HTTPS, follow no
//! redirect, which would carry the token elsewhere, and never reach an address of this
//! machine or its private network, whatever the URL's host resolves to: otherwise a URL
//! could have oxploy post to a service only it should reach, such as Caddy's admin API.
//!
//! oxploy speaks the protocol as a client: it opens a session, then lists the tools or
//! calls one. A server may answer each request with plain JSON or with an event stream,
//! and may give the session an ID to send back; oxploy handles both.

use std::fmt;
use std::io::{self, BufRead, BufReader};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;
use std::time::Duration;

use serde_json::{Value, json};
use toasty::Db;
use topcoat::{Result, context::Cx};

use super::deploy::{AppStatus, Deployer};
use super::project::{Project, now};
use super::service::Service;
use super::update::VERSION;
use crate::db::db;

/// The path an app serves MCP at unless the user says otherwise.
pub const DEFAULT_PATH: &str = "/mcp";

/// The most characters the path may have.
pub const MAX_PATH_LEN: usize = 200;

/// The most characters an external MCP server's URL may have.
pub const MAX_URL_LEN: usize = 500;

/// The most characters a token an external MCP server takes may have.
pub const MAX_TOKEN_LEN: usize = 512;

/// The most characters a tool's name may have, as the protocol suggests.
const MAX_TOOL_NAME_LEN: usize = 128;

/// The protocol version oxploy asks for. The server answers with the one it speaks.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// How long listing an app's tools may take, from connecting to the last byte.
const TIMEOUT: Duration = Duration::from_secs(5);

/// How long calling one of an app's tools may take, since a tool may have work to do.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// The most pages of tools oxploy reads, so a server that keeps sending more cannot
/// keep it busy.
const MAX_PAGES: usize = 20;

/// A tool of an app's MCP server that the user turned on. A tool without one is off,
/// as every tool is when the app first offers it.
#[derive(Debug, toasty::Model)]
pub struct McpTool {
    #[key]
    #[auto]
    pub id: u64,

    #[index]
    pub service_id: u64,

    /// The tool's name, as the app's server lists it.
    pub name: String,

    /// When the user turned it on, in seconds since the Unix epoch.
    pub enabled_at: i64,
}

impl McpTool {
    /// The names of the service's tools that are on.
    pub async fn enabled_of(cx: &Cx, service_id: u64) -> Result<Vec<String>> {
        let tools = McpTool::filter_by_service_id(service_id)
            .exec(&mut db(cx))
            .await?;
        Ok(tools.into_iter().map(|tool| tool.name).collect())
    }

    /// Turns the service's tool named `name` on or off. Returns `false`, changing
    /// nothing, if the name cannot be a tool's.
    pub async fn set(cx: &Cx, service_id: u64, name: &str, on: bool) -> Result<bool> {
        if !is_tool_name(name) {
            return Ok(false);
        }
        let mut db = db(cx);
        let existing: Vec<_> = McpTool::filter_by_service_id(service_id)
            .exec(&mut db)
            .await?
            .into_iter()
            .filter(|tool| tool.name == name)
            .collect();
        // A tool already on stays as it is, with when it was turned on.
        if on && !existing.is_empty() {
            return Ok(true);
        }
        for tool in existing {
            McpTool::filter_by_id(tool.id).delete().exec(&mut db).await?;
        }
        if on {
            toasty::create!(McpTool {
                service_id,
                name: name.to_owned(),
                enabled_at: now(),
            })
            .exec(&mut db)
            .await?;
        }
        Ok(true)
    }

    /// Forgets the tools of a service, as when it is deleted.
    pub async fn remove_all(db: &Db, service_id: u64) -> toasty::Result<()> {
        McpTool::filter_by_service_id(service_id)
            .delete()
            .exec(&mut db.clone())
            .await
    }
}

/// Whether `name` can be a tool's.
pub fn is_tool_name(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= MAX_TOOL_NAME_LEN
}

/// An app of an organization that serves MCP and has tools turned on.
pub struct Serving {
    pub service: Service,
    /// The name of the app's project.
    pub project: String,
    /// The names of the tools turned on in the app's MCP tab.
    pub enabled: Vec<String>,
}

/// The organization's apps and external MCP servers that serve MCP and have tools
/// turned on.
pub async fn serving(cx: &Cx, organization_id: u64) -> Result<Vec<Serving>> {
    let mut serving = Vec::new();
    for project in Project::in_organization(cx, organization_id).await? {
        for service in Service::in_project(cx, project.id).await? {
            if !service.serves_mcp() {
                continue;
            }
            let enabled = McpTool::enabled_of(cx, service.id).await?;
            if !enabled.is_empty() {
                serving.push(Serving {
                    service,
                    project: project.name.clone(),
                    enabled,
                });
            }
        }
    }
    Ok(serving)
}

/// Lists the tools of each app's MCP server, asking them all at once, so a slow one
/// only delays the answer by its own time. An app that is not running gets `None`.
pub async fn list_each(cx: &Cx, apps: Vec<Serving>) -> Vec<(Serving, Option<Result<Listing, ConnectError>>)> {
    let mut asked = Vec::new();
    for app in apps {
        let listing = endpoint(cx, &app.service)
            .map(|endpoint| tokio::spawn(async move { list_tools(&endpoint).await }));
        asked.push((app, listing));
    }

    let mut listed = Vec::new();
    for (app, listing) in asked {
        let listing = match listing {
            Some(listing) => Some(
                listing
                    .await
                    .unwrap_or_else(|error| Err(ConnectError::Unreachable(error.to_string()))),
            ),
            None => None,
        };
        listed.push((app, listing));
    }
    listed
}

/// Where oxploy reaches a service's MCP server, and the token it sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    pub token: String,
    /// Whether it runs elsewhere, so the calls go through [`EXTERNAL`] and
    /// [`EXTERNAL_CALLS`].
    pub external: bool,
}

/// Where oxploy reaches the service's MCP server now: an external MCP server's URL, or
/// an app's port on 127.0.0.1 while it runs. `None` when it serves no MCP, or is an app
/// that is not running.
pub fn endpoint(cx: &Cx, service: &Service) -> Option<Endpoint> {
    if service.is_external() {
        let (url, token) = (service.mcp_url.clone()?, service.mcp_token.clone()?);
        return Some(Endpoint { url, token, external: true });
    }
    let (port, path, token) = (service.port?, service.mcp_path.as_deref()?, service.mcp_token_in_use()?);
    (Deployer::get(cx).status(service) == AppStatus::Running).then(|| Endpoint {
        url: format!("http://127.0.0.1:{port}{path}"),
        token: token.to_owned(),
        external: false,
    })
}

/// An external MCP server's URL as oxploy keeps it, trimmed, or `None` if oxploy does
/// not call it: an `https` URL with a host, without a user or a fragment, of up to
/// [`MAX_URL_LEN`] characters. Where its host resolves is checked on each call.
pub fn external_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.len() > MAX_URL_LEN || url.contains('#') || url.chars().any(char::is_whitespace) {
        return None;
    }
    let uri: ureq::http::Uri = url.parse().ok()?;
    let authority = uri.authority()?;
    // What follows the host, which is a port when there is any.
    let after_host = authority.as_str().strip_prefix(authority.host()).unwrap_or(":");
    let port_is_valid = after_host.is_empty()
        || after_host
            .strip_prefix(':')
            .is_some_and(|port| port.parse::<u16>().is_ok_and(|port| port != 0));
    let valid = uri.scheme_str() == Some("https")
        && !authority.host().is_empty()
        && !authority.as_str().contains('@')
        && port_is_valid;
    valid.then(|| url.to_owned())
}

/// Whether `token` can be one an external MCP server takes, sent in a header: from 1
/// to [`MAX_TOKEN_LEN`] visible ASCII characters.
pub fn is_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN_LEN && token.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether oxploy may call `ip` for an external MCP server: an address on the public
/// internet, not one of this machine or of a private, shared or reserved network.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(mapped) => is_public_v4(mapped),
            None => is_public_v6(ip),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        // Shared address space, as behind a carrier's NAT.
        || (a == 100 && (64..128).contains(&b))
        // Protocol assignments.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking.
        || (a == 198 && (b == 18 || b == 19))
        // Reserved.
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // Unique local addresses.
        || (first & 0xfe00) == 0xfc00
        // Link-local addresses.
        || (first & 0xffc0) == 0xfe80
        // Documentation.
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        // IPv4-compatible and other addresses under ::/96.
        || ip.segments()[..6] == [0; 6])
}

/// Resolves hosts as `ureq` does, keeping only the public addresses, so a call to an
/// external MCP server never reaches this machine or its private network.
#[derive(Debug, Default)]
struct PublicOnly(ureq::unversioned::resolver::DefaultResolver);

impl ureq::unversioned::resolver::Resolver for PublicOnly {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let resolved = self.0.resolve(uri, config, timeout)?;
        let mut public = self.empty();
        for address in resolved.iter().filter(|address| is_public(address.ip())) {
            public.push(*address);
        }
        if public.is_empty() {
            return Err(ureq::Error::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "its host is an address of this machine or of a private network, which oxploy does not call",
            )));
        }
        Ok(public)
    }
}

/// An agent for external MCP servers, which waits up to `timeout`.
fn external_agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .user_agent("oxploy")
        .https_only(true)
        .max_redirects(0)
        .proxy(None)
        .build();
    ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        PublicOnly::default(),
    )
}

/// Calls external MCP servers.
static EXTERNAL: LazyLock<ureq::Agent> = LazyLock::new(|| external_agent(TIMEOUT));

/// Calls a tool of an external MCP server, which may take longer than listing.
static EXTERNAL_CALLS: LazyLock<ureq::Agent> = LazyLock::new(|| external_agent(CALL_TIMEOUT));

/// Whether `path` can be where an app serves MCP: a path below the root, such as
/// `/mcp` or `/api/mcp`. The root would hide the whole app from its domains.
pub fn is_path(path: &str) -> bool {
    path.len() > 1
        && path.len() <= MAX_PATH_LEN
        && path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains("//")
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~'))
}

/// A new token for an app's MCP server: random bytes in hex.
pub fn new_token() -> String {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).expect("the system provides random bytes");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// What an app's MCP server says about itself and its tools.
#[derive(Debug)]
pub struct Listing {
    pub server: Server,
    /// The protocol version the server speaks, such as `2025-06-18`.
    pub protocol_version: String,
    pub tools: Vec<Tool>,
}

/// An MCP server, as it names itself.
#[derive(Debug)]
pub struct Server {
    pub name: String,
    pub title: Option<String>,
    pub version: Option<String>,
}

/// A tool an app's MCP server offers.
#[derive(Debug, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub effect: Effect,
    /// The tool as the server listed it, with its arguments' schema, to pass on.
    pub definition: Value,
}

/// What a tool does to the app's data, as the app declares it. The protocol takes a
/// tool that declares nothing to be destructive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    ReadOnly,
    /// It changes data without destroying any, which the protocol takes to mean it only
    /// adds to it.
    Writes,
    Destructive,
}

/// Why oxploy could not list an app's tools.
#[derive(Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// Nothing answered, or the connection broke, with what went wrong.
    Unreachable(String),
    /// The app answered 401 or 403: it did not take oxploy's token.
    Refused(u16),
    /// The app answered 404 or 405: it serves no MCP at the path.
    NotFound,
    /// The app answered something other than MCP, with what was wrong.
    NotMcp(String),
    /// The app's server answered a request with a JSON-RPC error.
    Rpc {
        method: String,
        code: i64,
        message: String,
    },
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(error) => write!(f, "the MCP server could not be reached: {error}"),
            Self::Refused(status) => write!(f, "the MCP server turned oxploy away with {status}"),
            Self::NotFound => f.write_str("nothing serves MCP at this address"),
            Self::NotMcp(problem) => write!(f, "the answer is not MCP: {problem}"),
            Self::Rpc { method, message, .. } => {
                write!(f, "the MCP server answered {method} with an error: {message}")
            }
        }
    }
}

/// Lists the tools of the MCP server at `endpoint`.
pub async fn list_tools(endpoint: &Endpoint) -> Result<Listing, ConnectError> {
    let endpoint = endpoint.clone();
    // `ureq` blocks, so the calls run off the async threads.
    tokio::task::spawn_blocking(move || fetch_tools(&endpoint))
        .await
        .unwrap_or_else(|error| Err(ConnectError::Unreachable(error.to_string())))
}

/// Calls the tool `name` of the MCP server at `endpoint` with `arguments`, and returns
/// the result as the server gave it: its content, and whether the tool failed. The
/// server's own JSON-RPC errors come back as [`ConnectError::Rpc`].
pub async fn call_tool(endpoint: &Endpoint, name: &str, arguments: Value) -> Result<Value, ConnectError> {
    let (endpoint, name) = (endpoint.clone(), name.to_owned());
    tokio::task::spawn_blocking(move || fetch_call(&endpoint, &name, arguments))
        .await
        .unwrap_or_else(|error| Err(ConnectError::Unreachable(error.to_string())))
}

/// Opens a session with the server at `endpoint`, lists its tools and closes the
/// session.
fn fetch_tools(endpoint: &Endpoint) -> Result<Listing, ConnectError> {
    let agent = if endpoint.external { &EXTERNAL } else { &AGENT };
    let mut session = Session::new(agent, &endpoint.url, &endpoint.token);
    let listing = session.list();
    session.close();
    listing
}

/// Opens a session with the server at `endpoint`, calls one tool and closes the
/// session.
fn fetch_call(endpoint: &Endpoint, name: &str, arguments: Value) -> Result<Value, ConnectError> {
    let agent = if endpoint.external { &EXTERNAL_CALLS } else { &CALLS };
    let mut session = Session::new(agent, &endpoint.url, &endpoint.token);
    let result = session
        .initialize()
        .and_then(|_| session.request("tools/call", json!({ "name": name, "arguments": arguments })));
    session.close();
    result
}

/// Calls an app's MCP server.
static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .user_agent("oxploy")
        .build()
        .into()
});

/// Calls a tool of an app's MCP server, which may take longer than listing.
static CALLS: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(CALL_TIMEOUT))
        .user_agent("oxploy")
        .build()
        .into()
});

/// A session with an app's MCP server.
struct Session<'a> {
    agent: &'static ureq::Agent,
    url: &'a str,
    token: &'a str,
    /// The ID the server gave the session, sent back with every later request.
    id: Option<String>,
    /// The protocol version agreed on, sent with every request after `initialize`.
    version: Option<String>,
    next_request: u64,
}

impl<'a> Session<'a> {
    fn new(agent: &'static ureq::Agent, url: &'a str, token: &'a str) -> Self {
        Session {
            agent,
            url,
            token,
            id: None,
            version: None,
            next_request: 1,
        }
    }

    /// Initializes the session and lists the tools, page by page.
    fn list(&mut self) -> Result<Listing, ConnectError> {
        let (server, version) = self.initialize()?;

        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self.request("tools/list", params)?;
            let listed = page
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| ConnectError::NotMcp("its answer to tools/list has no tools".to_owned()))?;
            tools.extend(listed.iter().filter_map(tool));
            cursor = page.get("nextCursor").and_then(Value::as_str).map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(Listing {
            server,
            protocol_version: version,
            tools,
        })
    }

    /// Opens the session, and returns the server and the protocol version it speaks.
    fn initialize(&mut self) -> Result<(Server, String), ConnectError> {
        let initialized = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "oxploy", "title": "oxploy", "version": VERSION },
            }),
        )?;
        let version = initialized
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| ConnectError::NotMcp("its answer to initialize has no protocol version".to_owned()))?
            .to_owned();
        let info = initialized.get("serverInfo");
        let text = |field: &str| info.and_then(|info| info.get(field)).and_then(Value::as_str).map(str::to_owned);
        let server = Server {
            name: text("name").unwrap_or_default(),
            title: text("title"),
            version: text("version"),
        };
        self.version = Some(version.clone());
        self.notify("notifications/initialized")?;
        Ok((server, version))
    }

    /// Sends a request and returns its result.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, ConnectError> {
        let id = self.next_request;
        self.next_request += 1;
        let mut response = self.post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        if self.id.is_none() {
            self.id = response
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
        }
        let streams = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|kind| kind.to_ascii_lowercase().starts_with("text/event-stream"));
        let message = if streams {
            answer_in_stream(BufReader::new(response.body_mut().as_reader()), id)?
        } else {
            response
                .body_mut()
                .read_json::<Value>()
                .map_err(|error| ConnectError::NotMcp(format!("its answer to {method} is not JSON: {error}")))?
        };
        if let Some(error) = message.get("error") {
            return Err(ConnectError::Rpc {
                method: method.to_owned(),
                code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("no details")
                    .to_owned(),
            });
        }
        message
            .get("result")
            .cloned()
            .ok_or_else(|| ConnectError::NotMcp(format!("its answer to {method} has no result")))
    }

    /// Sends a notification, which gets no answer.
    fn notify(&mut self, method: &str) -> Result<(), ConnectError> {
        self.post(&json!({ "jsonrpc": "2.0", "method": method })).map(drop)
    }

    /// Posts one message, and turns away an answer whose status says it failed.
    fn post(&self, message: &Value) -> Result<ureq::http::Response<ureq::Body>, ConnectError> {
        let mut request = self
            .agent
            .post(self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Authorization", format!("Bearer {}", self.token));
        if let Some(id) = &self.id {
            request = request.header("Mcp-Session-Id", id);
        }
        if let Some(version) = &self.version {
            request = request.header("MCP-Protocol-Version", version);
        }
        let response = request
            .send_json(message)
            .map_err(|error| ConnectError::Unreachable(error.to_string()))?;
        match response.status().as_u16() {
            200..=299 => Ok(response),
            status @ (401 | 403) => Err(ConnectError::Refused(status)),
            404 | 405 => Err(ConnectError::NotFound),
            status => Err(ConnectError::NotMcp(format!("it answered {status}"))),
        }
    }

    /// Ends the session, if the server gave it an ID. What the server answers does not
    /// matter: it ends the session on its own after a while anyway.
    fn close(&self) {
        if let Some(id) = &self.id {
            let mut request = self
                .agent
                .delete(self.url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Mcp-Session-Id", id);
            if let Some(version) = &self.version {
                request = request.header("MCP-Protocol-Version", version);
            }
            let _ = request.call();
        }
    }
}

/// Reads an event stream until the answer to request `id`, skipping the events before
/// it, such as notifications. Reading stops there, as a server may keep the stream open.
fn answer_in_stream(stream: impl BufRead, id: u64) -> Result<Value, ConnectError> {
    let answers = |data: &str| {
        serde_json::from_str::<Value>(data)
            .ok()
            .filter(|message| message.get("id").and_then(Value::as_u64) == Some(id))
    };
    let mut data = String::new();
    for line in stream.lines() {
        let line = line.map_err(|error| ConnectError::Unreachable(error.to_string()))?;
        if line.is_empty() {
            if let Some(message) = answers(&data) {
                return Ok(message);
            }
            data.clear();
        } else if let Some(field) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(field.strip_prefix(' ').unwrap_or(field));
        }
    }
    answers(&data).ok_or_else(|| ConnectError::NotMcp("its event stream ended without an answer".to_owned()))
}

/// A tool as `tools/list` describes it, or `None` without a name.
fn tool(value: &Value) -> Option<Tool> {
    let name = value.get("name")?.as_str()?.to_owned();
    let annotations = value.get("annotations");
    let hint = |field: &str| annotations.and_then(|hints| hints.get(field)).and_then(Value::as_bool);
    let text = |field: &str| value.get(field).and_then(Value::as_str).map(str::to_owned);
    let effect = match (hint("readOnlyHint"), hint("destructiveHint")) {
        (Some(true), _) => Effect::ReadOnly,
        (_, Some(false)) => Effect::Writes,
        _ => Effect::Destructive,
    };
    Some(Tool {
        name,
        title: text("title").or_else(|| {
            annotations
                .and_then(|hints| hints.get("title"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        description: text("description"),
        effect,
        definition: value.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// An app's MCP server at `url`, which takes `token`.
    fn local(url: &str, token: &str) -> Endpoint {
        Endpoint { url: url.to_owned(), token: token.to_owned(), external: false }
    }

    /// An external MCP server at `url`.
    fn external(url: &str) -> Endpoint {
        Endpoint { url: url.to_owned(), token: "secret".to_owned(), external: true }
    }

    /// A request the fake server got: its method, headers in lowercase, and body.
    #[derive(Clone, Debug)]
    struct Got {
        method: String,
        headers: Vec<(String, String)>,
        body: Value,
    }

    impl Got {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }

        fn rpc(&self) -> &str {
            self.body.get("method").and_then(Value::as_str).unwrap_or("")
        }
    }

    /// An answer of the fake server: status, extra headers and body.
    type Answer = (u16, Vec<(&'static str, String)>, String);

    /// Serves `answer` for every request on a port of its own, and returns the server's
    /// URL with what it got.
    fn serve(answer: impl Fn(&Got) -> Answer + Send + 'static) -> (String, Arc<Mutex<Vec<Got>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let got = Arc::new(Mutex::new(Vec::new()));
        let seen = got.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut headers = Vec::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    let (key, value) = line.split_once(':').unwrap();
                    headers.push((key.trim().to_ascii_lowercase(), value.trim().to_owned()));
                }
                let length: usize = headers
                    .iter()
                    .find(|(key, _)| key == "content-length")
                    .map_or(0, |(_, value)| value.parse().unwrap());
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request = Got {
                    method: request_line.split(' ').next().unwrap().to_owned(),
                    headers,
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                };
                let (status, extra, body) = answer(&request);
                seen.lock().unwrap().push(request);
                let mut response = format!("HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
                for (key, value) in extra {
                    response.push_str(&format!("{key}: {value}\r\n"));
                }
                response.push_str("\r\n");
                response.push_str(&body);
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (url, got)
    }

    fn json_answer(got: &Got, result: Value) -> Answer {
        let body = json!({ "jsonrpc": "2.0", "id": got.body["id"], "result": result });
        (200, vec![("Content-Type", "application/json".to_owned())], body.to_string())
    }

    fn initialized() -> Value {
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "portaglobal", "title": "Porta Global", "version": "0.1.7" },
        })
    }

    #[test]
    fn lists_the_tools_of_a_server_that_answers_json() {
        let (url, got) = serve(|got| match got.rpc() {
            "initialize" => json_answer(got, initialized()),
            "tools/list" => json_answer(got, json!({ "tools": [
                { "name": "list_requests", "title": "List quote requests", "description": "Lists them.",
                  "annotations": { "readOnlyHint": true } },
                { "name": "archive", "annotations": { "destructiveHint": false } },
                { "name": "purge" },
                { "title": "No name" },
            ] })),
            _ => (202, Vec::new(), String::new()),
        });

        let listing = fetch_tools(&local(&url, "secret")).unwrap();
        assert_eq!(listing.server.name, "portaglobal");
        assert_eq!(listing.server.title.as_deref(), Some("Porta Global"));
        assert_eq!(listing.protocol_version, "2025-06-18");
        let tools: Vec<_> = listing
            .tools
            .iter()
            .map(|tool| (tool.name.as_str(), tool.title.as_deref(), tool.description.as_deref(), tool.effect))
            .collect();
        assert_eq!(
            tools,
            [
                ("list_requests", Some("List quote requests"), Some("Lists them."), Effect::ReadOnly),
                ("archive", None, None, Effect::Writes),
                ("purge", None, None, Effect::Destructive),
            ]
        );
        assert_eq!(listing.tools[1].definition, json!({ "name": "archive", "annotations": { "destructiveHint": false } }));

        let got = got.lock().unwrap();
        let methods: Vec<_> = got.iter().map(Got::rpc).collect();
        assert_eq!(methods, ["initialize", "notifications/initialized", "tools/list"]);
        assert!(got.iter().all(|got| got.header("authorization") == Some("Bearer secret")));
        assert!(got.iter().all(|got| got.header("accept") == Some("application/json, text/event-stream")));
        assert_eq!(got[0].header("mcp-protocol-version"), None);
        assert_eq!(got[2].header("mcp-protocol-version"), Some("2025-06-18"));
    }

    #[test]
    fn follows_a_session_through_event_streams_and_pages() {
        let (url, got) = serve(|got| {
            let stream = |result: Value| {
                // A notification comes first, which the client skips.
                let note = json!({ "jsonrpc": "2.0", "method": "notifications/message", "params": {} });
                let answer = json!({ "jsonrpc": "2.0", "id": got.body["id"], "result": result });
                let body = format!("event: message\ndata: {note}\n\n: a comment\nevent: message\ndata: {answer}\n\n");
                (200, vec![("Content-Type", "text/event-stream".to_owned()), ("Mcp-Session-Id", "s-1".to_owned())], body)
            };
            match (got.method.as_str(), got.rpc()) {
                ("DELETE", _) => (200, Vec::new(), String::new()),
                (_, "initialize") => stream(initialized()),
                (_, "tools/list") if got.body["params"]["cursor"].is_null() => {
                    stream(json!({ "tools": [{ "name": "first" }], "nextCursor": "2" }))
                }
                (_, "tools/list") => stream(json!({ "tools": [{ "name": "second" }] })),
                _ => (202, Vec::new(), String::new()),
            }
        });

        let listing = fetch_tools(&local(&url, "secret")).unwrap();
        let names: Vec<_> = listing.tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["first", "second"]);

        let got = got.lock().unwrap();
        assert_eq!(got[0].header("mcp-session-id"), None);
        assert!(got[1..].iter().all(|got| got.header("mcp-session-id") == Some("s-1")));
        assert_eq!(got[3].body["params"]["cursor"], "2");
        assert_eq!(got.last().unwrap().method, "DELETE");
    }

    #[test]
    fn says_why_it_could_not_list() {
        let (refusing, _) = serve(|_| (401, Vec::new(), "Unauthorized".to_owned()));
        assert_eq!(fetch_tools(&local(&refusing, "wrong")).unwrap_err(), ConnectError::Refused(401));

        let (missing, _) = serve(|_| (404, Vec::new(), "Not found".to_owned()));
        assert_eq!(fetch_tools(&local(&missing, "secret")).unwrap_err(), ConnectError::NotFound);

        let (page, _) = serve(|_| (200, vec![("Content-Type", "text/html".to_owned())], "<html>".to_owned()));
        assert!(matches!(fetch_tools(&local(&page, "secret")), Err(ConnectError::NotMcp(_))));

        let (failing, _) = serve(|got| {
            let body = json!({ "jsonrpc": "2.0", "id": got.body["id"], "error": { "code": -32603, "message": "boom" } });
            (200, vec![("Content-Type", "application/json".to_owned())], body.to_string())
        });
        assert_eq!(
            fetch_tools(&local(&failing, "secret")).unwrap_err(),
            ConnectError::Rpc { method: "initialize".to_owned(), code: -32603, message: "boom".to_owned() }
        );

        // A port nothing listens on: bound, then let go.
        let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(matches!(
            fetch_tools(&local(&format!("http://{free}/mcp"), "secret")),
            Err(ConnectError::Unreachable(_))
        ));
    }

    #[test]
    fn calls_a_tool_and_returns_its_result_as_it_came() {
        let (url, got) = serve(|got| match got.rpc() {
            "initialize" => json_answer(got, initialized()),
            "tools/call" if got.body["params"]["name"] == "list_requests" => json_answer(
                got,
                json!({ "content": [{ "type": "text", "text": "{\"count\":0}" }], "isError": false }),
            ),
            "tools/call" => {
                let body = json!({ "jsonrpc": "2.0", "id": got.body["id"], "error": { "code": -32602, "message": "Unknown tool" } });
                (200, vec![("Content-Type", "application/json".to_owned())], body.to_string())
            }
            _ => (202, Vec::new(), String::new()),
        });

        let result = fetch_call(&local(&url, "secret"), "list_requests", json!({ "stage": "new" })).unwrap();
        assert_eq!(result["content"][0]["text"], "{\"count\":0}");
        let sent = got.lock().unwrap().iter().find(|got| got.rpc() == "tools/call").cloned().unwrap();
        assert_eq!(sent.body["params"], json!({ "name": "list_requests", "arguments": { "stage": "new" } }));
        assert_eq!(sent.header("authorization"), Some("Bearer secret"));

        assert_eq!(
            fetch_call(&local(&url, "secret"), "delete_all", json!({})).unwrap_err(),
            ConnectError::Rpc { method: "tools/call".to_owned(), code: -32602, message: "Unknown tool".to_owned() }
        );
    }

    #[test]
    fn takes_https_urls_with_a_host_for_external_servers() {
        for url in [
            "https://api.example.com/mcp",
            " https://mcp.example.com:8443/v1?team=a ",
            "https://example.com",
            "https://[2606:4700::1111]:8443/mcp",
        ] {
            assert_eq!(external_url(url).as_deref(), Some(url.trim()), "{url}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN));
        for url in [
            "http://api.example.com/mcp",
            "ftp://example.com/mcp",
            "https:///mcp",
            "https://user:pass@example.com/mcp",
            "https://example.com/mcp#tools",
            "https://exa mple.com/mcp",
            "https://example.com:x/mcp",
            "https://example.com:0/mcp",
            "https://example.com:70000/mcp",
            "api.example.com/mcp",
            "",
            &long,
        ] {
            assert_eq!(external_url(url), None, "{url}");
        }
    }

    #[test]
    fn takes_tokens_that_fit_in_a_header() {
        assert!(is_token("oxp_3f9a1c"));
        assert!(is_token("sk-live.A1/b2+c3=="));
        assert!(is_token(&"a".repeat(MAX_TOKEN_LEN)));
        for token in ["", "two words", "line\nbreak", "ção", &"a".repeat(MAX_TOKEN_LEN + 1)] {
            assert!(!is_token(token), "{token}");
        }
    }

    #[test]
    fn calls_only_public_addresses_for_external_servers() {
        for public in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111", "::ffff:93.184.216.34"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
        for private in [
            "127.0.0.1", "10.0.0.5", "172.16.3.4", "192.168.1.10", "169.254.169.254", "0.0.0.0",
            "100.64.0.1", "255.255.255.255", "224.0.0.1", "240.0.0.1", "::1", "::", "fc00::1",
            "fd12:3456::1", "fe80::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "2001:db8::1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
    }

    #[test]
    fn refuses_external_servers_on_this_machine_before_connecting() {
        let (url, got) = serve(|got| json_answer(got, initialized()));
        let address = url.trim_start_matches("http://");

        let refused = fetch_tools(&external(&format!("https://{address}"))).unwrap_err();
        assert!(
            matches!(&refused, ConnectError::Unreachable(why) if why.contains("private network")),
            "{refused:?}"
        );
        let port = address.split([':', '/']).nth(1).unwrap();
        let refused = fetch_tools(&external(&format!("https://localhost:{port}/mcp"))).unwrap_err();
        assert!(matches!(refused, ConnectError::Unreachable(_)), "{refused:?}");
        // Plain HTTP goes nowhere either.
        assert!(fetch_tools(&external(&url)).is_err());
        assert!(got.lock().unwrap().is_empty());
    }

    #[test]
    fn takes_paths_below_the_root() {
        for path in ["/mcp", "/api/mcp", "/v1.0/mcp-server", "/_mcp"] {
            assert!(is_path(path), "{path}");
        }
        for path in ["", "/", "mcp", "/mcp/", "//mcp", "/mcp?x=1", "/m cp", "/mcp#", &format!("/{}", "a".repeat(MAX_PATH_LEN))] {
            assert!(!is_path(path), "{path}");
        }
    }
}
