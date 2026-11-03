//! Caddy, the web server in front of the apps. It takes the requests for their domains
//! on ports 80 and 443, over HTTPS with certificates it gets and renews, and passes
//! them to each app on 127.0.0.1.
//!
//! Caddy runs on its own, as a service of the machine, and belongs to oxploy: oxploy
//! builds its whole configuration from the database and loads it through Caddy's admin
//! API. It checks every few seconds that Caddy still runs that configuration, and
//! loads it again when Caddy lost it, as after a restart, or when something changed.
//!
//! The panel also answers at the server's public address, with a short-lived
//! certificate for the address, so a fresh install can be set up over HTTPS before it
//! has a domain.

use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Notify;
use toasty::Db;
use topcoat::context::{Cx, app_context};

use super::domain::{self, Domain, PanelDomain};
use super::service::Service;

/// How often oxploy checks that Caddy runs its configuration.
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Where Caddy's admin API listens when `OXPLOY_CADDY_ADMIN` does not say.
const DEFAULT_ADMIN: &str = "127.0.0.1:2019";

/// Talks to Caddy's admin API, which answers right away when it runs.
static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into()
});

/// Where Caddy listens, and where the panel does, from oxploy's environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ports {
    /// The admin API's address, such as `127.0.0.1:2019`.
    pub admin: String,
    pub http: u16,
    pub https: u16,
    /// Where Caddy reaches the panel, such as `127.0.0.1:3000`.
    pub panel: String,
    /// The server's public addresses, where the panel answers over HTTPS with or
    /// without a domain.
    pub public_ips: Vec<IpAddr>,
}

impl Ports {
    /// `OXPLOY_CADDY_ADMIN`, `OXPLOY_HTTP_PORT` and `OXPLOY_HTTPS_PORT`, or 127.0.0.1:2019,
    /// 80 and 443. The panel listens at `HOST` and `PORT`, as Topcoat reads them. The
    /// public addresses are `OXPLOY_PUBLIC_IP`, separated by commas, for a server
    /// behind NAT, or else the machine's own public IPv4 addresses.
    fn from_env() -> Ports {
        let port = |name: &str, default: u16| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(default)
        };
        Ports {
            admin: std::env::var("OXPLOY_CADDY_ADMIN").unwrap_or_else(|_| DEFAULT_ADMIN.to_owned()),
            http: port("OXPLOY_HTTP_PORT", 80),
            https: port("OXPLOY_HTTPS_PORT", 443),
            panel: panel_address(
                std::env::var("HOST").ok().as_deref(),
                port("PORT", 3000),
            ),
            public_ips: match std::env::var("OXPLOY_PUBLIC_IP") {
                Ok(listed) => listed
                    .split(',')
                    .filter_map(|address| address.trim().parse().ok())
                    .collect(),
                Err(_) => domain::public_ipv4_addresses(),
            },
        }
    }

    /// The addresses the panel opens at by the server's public addresses, such as
    /// `https://203.0.114.7`.
    pub fn panel_urls(&self) -> Vec<String> {
        let port = if self.https == 443 {
            String::new()
        } else {
            format!(":{}", self.https)
        };
        self.public_ips
            .iter()
            .map(|address| match address {
                IpAddr::V4(_) => format!("https://{address}{port}"),
                IpAddr::V6(_) => format!("https://[{address}]{port}"),
            })
            .collect()
    }
}

/// Where Caddy reaches a panel that listens at `host` and `port`: on 127.0.0.1 when it
/// listens on every address.
fn panel_address(host: Option<&str>, port: u16) -> String {
    match host.map(|host| host.trim_start_matches('[').trim_end_matches(']')) {
        None | Some("" | "0.0.0.0" | "::") => format!("127.0.0.1:{port}"),
        Some(host) if host.contains(':') => format!("[{host}]:{port}"),
        Some(host) => format!("{host}:{port}"),
    }
}

/// Whether Caddy runs oxploy's configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaddyStatus {
    /// Not checked yet.
    Unknown,
    /// Caddy runs it.
    Running,
    /// Nothing answers at the admin API's address, with what went wrong.
    Unreachable(String),
    /// Caddy answered but would not load the configuration, with what it said.
    Refused(String),
}

/// Caddy, as oxploy sees it, shared by every request.
#[derive(Clone)]
pub struct Caddy {
    shared: Arc<Shared>,
}

struct Shared {
    db: Db,
    ports: Ports,
    status: Mutex<CaddyStatus>,
    changed: Notify,
}

impl Caddy {
    /// Starts keeping Caddy's configuration in step with the database, on a task of its
    /// own.
    pub fn start(db: Db) -> Caddy {
        let caddy = Caddy {
            shared: Arc::new(Shared {
                db,
                ports: Ports::from_env(),
                status: Mutex::new(CaddyStatus::Unknown),
                changed: Notify::new(),
            }),
        };
        let shared = caddy.shared.clone();
        tokio::spawn(async move {
            loop {
                let status = sync(&shared).await;
                *shared.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
                tokio::select! {
                    () = shared.changed.notified() => {}
                    () = tokio::time::sleep(CHECK_INTERVAL) => {}
                }
            }
        });
        caddy
    }

    /// Returns Caddy as registered on the router.
    pub fn get(cx: &Cx) -> &Caddy {
        app_context::<Caddy>(cx)
    }

    pub fn ports(&self) -> &Ports {
        &self.shared.ports
    }

    pub fn status(&self) -> CaddyStatus {
        self.shared
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Loads the configuration again soon, after the domains changed.
    pub fn changed(&self) {
        self.shared.changed.notify_one();
    }
}

/// Builds the configuration from the database and loads it into Caddy, unless Caddy
/// already runs it.
async fn sync(shared: &Shared) -> CaddyStatus {
    let sites = match sites(&shared.db).await {
        Ok(sites) => sites,
        // The database is oxploy's own; Caddy keeps what it runs meanwhile.
        Err(error) => return CaddyStatus::Refused(format!("the domains could not be read: {error}")),
    };
    let wanted = config(&shared.ports, &sites);
    let admin = shared.ports.admin.clone();
    // `ureq` blocks, so the requests run off the async threads.
    let call = move || {
        let mut current = AGENT
            .get(format!("http://{admin}/config/"))
            .call()
            .map_err(|error| CaddyStatus::Unreachable(error.to_string()))?;
        if current.status().is_success()
            && current.body_mut().read_json::<Value>().is_ok_and(|current| current == wanted)
        {
            return Ok(());
        }
        let mut loaded = AGENT
            .post(format!("http://{admin}/load"))
            .send_json(&wanted)
            .map_err(|error| CaddyStatus::Unreachable(error.to_string()))?;
        if loaded.status().is_success() {
            return Ok(());
        }
        let body = loaded.body_mut().read_to_string().unwrap_or_default();
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|answer| answer["error"].as_str().map(str::to_owned))
            .unwrap_or(body);
        Err(CaddyStatus::Refused(message.trim().to_owned()))
    };
    match tokio::task::spawn_blocking(call).await {
        Ok(Ok(())) => CaddyStatus::Running,
        Ok(Err(status)) => status,
        Err(error) => CaddyStatus::Unreachable(error.to_string()),
    }
}

/// Where a domain's requests go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upstream {
    /// To the app, at this port on 127.0.0.1.
    App(u64),
    /// Nowhere yet: the service has never been deployed.
    NotDeployed,
    /// Nowhere: the user stopped the service.
    Stopped,
    /// To the panel itself.
    Panel,
}

/// A domain with where its requests go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub host: String,
    pub path: Option<String>,
    pub strip_path: bool,
    pub https: bool,
    pub redirect_www: bool,
    pub upstream: Upstream,
}

/// Every domain on the server, the panel's included, with where its requests go.
async fn sites(db: &Db) -> toasty::Result<Vec<Site>> {
    let domains = Domain::all_in(db).await?;
    let services = Service::all().exec(&mut db.clone()).await?;
    let panel = PanelDomain::get_in(db).await?.map(|panel| Site {
        host: panel.host,
        path: None,
        strip_path: false,
        https: true,
        redirect_www: false,
        upstream: Upstream::Panel,
    });
    Ok(domains
        .into_iter()
        .filter_map(|domain| {
            let service = services.iter().find(|service| service.id == domain.service_id)?;
            let upstream = match (service.port, service.release_id, service.stopped_at) {
                (_, _, Some(_)) => Upstream::Stopped,
                (Some(port), Some(_), None) => Upstream::App(port),
                _ => Upstream::NotDeployed,
            };
            Some(Site {
                host: domain.host,
                path: domain.path,
                strip_path: domain.strip_path,
                https: domain.https,
                redirect_www: domain.redirect_www,
                upstream,
            })
        })
        .chain(panel)
        .collect())
}

/// The configuration Caddy runs for `sites`: a server on the HTTPS port for the domains
/// with HTTPS, whose certificates and redirects from HTTP Caddy manages on its own, and
/// one on the HTTP port for the others. On each host, longer paths come first, and a
/// host with only paths answers the others with 404, which Caddy would otherwise
/// answer with an empty page or a redirect to HTTPS. The panel answers at the server's
/// public addresses too.
pub fn config(ports: &Ports, sites: &[Site]) -> Value {
    let by_address: Vec<Site> = ports
        .public_ips
        .iter()
        .map(|address| Site {
            host: address.to_string(),
            path: None,
            strip_path: false,
            https: true,
            redirect_www: false,
            upstream: Upstream::Panel,
        })
        .collect();
    let mut sites: Vec<&Site> = sites.iter().chain(&by_address).collect();
    sites.sort_by(|a, b| {
        let depth = |site: &Site| site.path.as_deref().map_or(0, str::len);
        a.host.cmp(&b.host).then(depth(b).cmp(&depth(a)))
    });
    let mut servers = serde_json::Map::new();
    for (name, https, port) in [("https", true, ports.https), ("http", false, ports.http)] {
        let mut routes = Vec::new();
        let on_server: Vec<&Site> = sites.iter().copied().filter(|site| site.https == https).collect();
        for (index, site) in on_server.iter().enumerate() {
            routes.extend(self::routes(site, ports));
            let last_of_host = on_server
                .get(index + 1)
                .is_none_or(|next| next.host != site.host);
            if last_of_host && site.path.is_some() {
                routes.push(json!({
                    "match": [{ "host": [site.host] }],
                    "handle": [{ "handler": "static_response", "status_code": 404 }],
                    "terminal": true,
                }));
            }
        }
        if !routes.is_empty() {
            servers.insert(
                name.to_owned(),
                json!({ "listen": [format!(":{port}")], "routes": routes }),
            );
        }
    }
    let mut config = json!({
        "admin": { "listen": ports.admin },
        "apps": {
            "http": {
                "http_port": ports.http,
                "https_port": ports.https,
                "servers": servers,
            },
            // Names such as shop.localhost get certificates from Caddy's own authority,
            // which Caddy would otherwise add to the system's trusted ones, asking for
            // a password. `caddy trust` does it by hand.
            "pki": { "certificate_authorities": { "local": { "install_trust": false } } },
        }
    });
    let policies = address_policies(&ports.public_ips);
    if !policies.is_empty() {
        config["apps"]["tls"] = json!({ "automation": { "policies": policies } });
    }
    config
}

/// How the server's addresses get their certificates. Let's Encrypt certifies a public
/// address only with its short-lived profile, of six days, which Caddy renews; should
/// that fail, Caddy's own authority signs one, which browsers warn about. Other
/// addresses, which no public authority certifies, get Caddy's own.
fn address_policies(addresses: &[IpAddr]) -> Vec<Value> {
    let (reachable, other): (Vec<&IpAddr>, Vec<&IpAddr>) = addresses
        .iter()
        .partition(|address| domain::is_reachable(address));
    let subjects = |addresses: &[&IpAddr]| -> Vec<String> {
        addresses.iter().map(ToString::to_string).collect()
    };
    let mut policies = Vec::new();
    if !reachable.is_empty() {
        policies.push(json!({
            "subjects": subjects(&reachable),
            "issuers": [{ "module": "acme", "profile": "shortlived" }, { "module": "internal" }],
        }));
    }
    if !other.is_empty() {
        policies.push(json!({
            "subjects": subjects(&other),
            "issuers": [{ "module": "internal" }],
        }));
    }
    policies
}

/// The routes of one domain: to its app, and from `www.` when it redirects.
fn routes(site: &Site, ports: &Ports) -> Vec<Value> {
    let mut matcher = json!({ "host": [site.host] });
    if let Some(path) = &site.path {
        matcher["path"] = json!([path, format!("{path}/*")]);
    }
    let mut handle = Vec::new();
    match site.upstream {
        Upstream::App(port) => {
            if let (Some(path), true) = (&site.path, site.strip_path) {
                handle.push(json!({ "handler": "rewrite", "strip_path_prefix": path }));
            }
            handle.push(json!({
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": format!("127.0.0.1:{port}") }],
            }));
        }
        Upstream::Panel => handle.push(json!({
            "handler": "reverse_proxy",
            "upstreams": [{ "dial": ports.panel }],
        })),
        Upstream::NotDeployed => handle.push(unavailable("This app has not been deployed yet.")),
        Upstream::Stopped => handle.push(unavailable("This app is stopped.")),
    }
    let mut routes = vec![json!({ "match": [matcher], "handle": handle, "terminal": true })];
    if site.redirect_www && site.path.is_none() {
        let (scheme, port, usual) = if site.https {
            ("https", ports.https, 443)
        } else {
            ("http", ports.http, 80)
        };
        let port = if port == usual { String::new() } else { format!(":{port}") };
        routes.push(json!({
            "match": [{ "host": [format!("www.{}", site.host)] }],
            "handle": [{
                "handler": "static_response",
                "status_code": 308,
                "headers": { "Location": [format!("{scheme}://{}{port}{{http.request.uri}}", site.host)] },
            }],
            "terminal": true,
        }));
    }
    routes
}

/// A plain 503 answer with `text`, for a domain whose app does not run.
fn unavailable(text: &str) -> Value {
    json!({
        "handler": "static_response",
        "status_code": 503,
        "headers": { "Content-Type": ["text/plain; charset=utf-8"] },
        "body": format!("{text}\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ports() -> Ports {
        Ports {
            admin: "127.0.0.1:2019".to_owned(),
            http: 80,
            https: 443,
            panel: "127.0.0.1:3000".to_owned(),
            public_ips: Vec::new(),
        }
    }

    fn site(host: &str, path: Option<&str>, https: bool, upstream: Upstream) -> Site {
        Site {
            host: host.to_owned(),
            path: path.map(str::to_owned),
            strip_path: false,
            https,
            redirect_www: false,
            upstream,
        }
    }

    #[test]
    fn https_and_plain_http_get_a_server_each() {
        let config = config(
            &ports(),
            &[
                site("shop.com", None, true, Upstream::App(10_001)),
                site("blog.test", None, false, Upstream::NotDeployed),
            ],
        );
        let servers = &config["apps"]["http"]["servers"];
        assert_eq!(servers["https"]["listen"], json!([":443"]));
        assert_eq!(servers["https"]["routes"][0]["match"][0]["host"], json!(["shop.com"]));
        assert_eq!(
            servers["https"]["routes"][0]["handle"][0]["upstreams"][0]["dial"],
            "127.0.0.1:10001"
        );
        assert_eq!(servers["http"]["listen"], json!([":80"]));
        assert_eq!(servers["http"]["routes"][0]["handle"][0]["status_code"], 503);
        assert_eq!(config["admin"]["listen"], "127.0.0.1:2019");
    }

    #[test]
    fn longer_paths_come_first_and_can_be_stripped() {
        let api = Site {
            strip_path: true,
            ..site("shop.com", Some("/api"), true, Upstream::App(10_002))
        };
        let config = config(&ports(), &[site("shop.com", None, true, Upstream::App(10_001)), api]);
        let routes = &config["apps"]["http"]["servers"]["https"]["routes"];
        assert_eq!(routes[0]["match"][0]["path"], json!(["/api", "/api/*"]));
        assert_eq!(routes[0]["handle"][0]["strip_path_prefix"], "/api");
        assert_eq!(routes[1]["match"][0].get("path"), None);
        assert_eq!(routes.as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn the_panel_is_reached_where_it_listens() {
        assert_eq!(panel_address(None, 3000), "127.0.0.1:3000");
        assert_eq!(panel_address(Some("0.0.0.0"), 8000), "127.0.0.1:8000");
        assert_eq!(panel_address(Some("10.0.0.5"), 3000), "10.0.0.5:3000");
        assert_eq!(panel_address(Some("::1"), 3000), "[::1]:3000");
        let config = config(&ports(), &[site("panel.com", None, true, Upstream::Panel)]);
        let route = &config["apps"]["http"]["servers"]["https"]["routes"][0];
        assert_eq!(route["handle"][0]["upstreams"][0]["dial"], "127.0.0.1:3000");
    }

    #[test]
    fn the_panel_answers_at_the_public_addresses() {
        let with_addresses = Ports {
            public_ips: vec!["203.0.114.7".parse().unwrap(), "10.0.0.5".parse().unwrap()],
            ..ports()
        };
        assert_eq!(with_addresses.panel_urls(), ["https://203.0.114.7", "https://10.0.0.5"]);
        let built = config(&with_addresses, &[]);
        // Hosts sort as text, so 10.0.0.5 comes first.
        let route = &built["apps"]["http"]["servers"]["https"]["routes"][1];
        assert_eq!(route["match"][0]["host"], json!(["203.0.114.7"]));
        assert_eq!(route["handle"][0]["upstreams"][0]["dial"], "127.0.0.1:3000");
        let policies = &built["apps"]["tls"]["automation"]["policies"];
        assert_eq!(policies[0]["subjects"], json!(["203.0.114.7"]));
        assert_eq!(policies[0]["issuers"][0]["profile"], "shortlived");
        assert_eq!(policies[1]["subjects"], json!(["10.0.0.5"]));
        assert_eq!(policies[1]["issuers"], json!([{ "module": "internal" }]));
        // Without public addresses, the configuration stays as it was.
        assert_eq!(config(&ports(), &[])["apps"].get("tls"), None);
    }

    #[test]
    fn hosts_with_only_paths_answer_404_elsewhere() {
        let config = config(&ports(), &[site("api.com", Some("/v1"), false, Upstream::App(10_001))]);
        let routes = &config["apps"]["http"]["servers"]["http"]["routes"];
        assert_eq!(routes[1]["match"][0], json!({ "host": ["api.com"] }));
        assert_eq!(routes[1]["handle"][0]["status_code"], 404);
    }

    #[test]
    fn www_redirects_to_the_host() {
        let config = config(
            &ports(),
            &[Site { redirect_www: true, ..site("shop.com", None, true, Upstream::Stopped) }],
        );
        let routes = &config["apps"]["http"]["servers"]["https"]["routes"];
        assert_eq!(routes[1]["match"][0]["host"], json!(["www.shop.com"]));
        assert_eq!(
            routes[1]["handle"][0]["headers"]["Location"],
            json!(["https://shop.com{http.request.uri}"])
        );
    }
}
