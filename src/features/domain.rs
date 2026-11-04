//! The domains that reach a service's app. Caddy takes their requests and passes them
//! to the app; see [`caddy`](super::caddy).

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use sysinfo::Networks;
use toasty::Db;
use topcoat::{Result, context::Cx};

use super::project::now;
use super::service::Service;
use crate::db::db;

/// A hostname, and maybe a path under it, that sends its requests to a service's app.
#[derive(Debug, toasty::Model)]
pub struct Domain {
    #[key]
    #[auto]
    pub id: u64,

    #[index]
    pub service_id: u64,

    /// The hostname, in lowercase, such as `shop.example.com`.
    pub host: String,

    /// The path the app answers under, such as `/api`, or `None` for the whole host.
    pub path: Option<String>,

    /// Whether the path is taken off before the request reaches the app.
    pub strip_path: bool,

    /// Whether the domain is served over HTTPS, with a certificate Caddy gets and renews.
    pub https: bool,

    /// Whether `www.` and the host sends visitors to the host.
    pub redirect_www: bool,

    /// When it was added, in seconds since the Unix epoch.
    pub created_at: i64,
}

/// The address the panel itself answers at, always over HTTPS, since its sign-in
/// cookie travels only over HTTPS away from localhost. There is at most one.
#[derive(Debug, toasty::Model)]
pub struct PanelDomain {
    #[key]
    #[auto]
    pub id: u64,

    /// The hostname, in lowercase, such as `panel.example.com`.
    pub host: String,

    /// When it was set, in seconds since the Unix epoch.
    pub created_at: i64,
}

/// A domain as the user filled the form.
pub struct NewDomain<'a> {
    pub host: &'a str,
    pub path: &'a str,
    pub strip_path: bool,
    pub https: bool,
    pub redirect_www: bool,
}

/// The most characters a hostname may have.
const MAX_HOST_LEN: usize = 253;

/// The most characters a path may have.
const MAX_PATH_LEN: usize = 255;

/// How long looking up a domain may take.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

/// The DNS service that generated domains go under.
const GENERATED_DOMAIN: &str = "sslip.io";

impl Domain {
    /// Returns the service's domains, oldest first.
    pub async fn of_service(cx: &Cx, service_id: u64) -> Result<Vec<Domain>> {
        Ok(Domain::filter_by_service_id(service_id)
            .order_by(Domain::fields().id().asc())
            .exec(&mut db(cx))
            .await?)
    }

    /// Returns every domain on the server.
    pub async fn all_in(db: &Db) -> toasty::Result<Vec<Domain>> {
        Domain::all().exec(&mut db.clone()).await
    }

    /// Returns the domain with this ID if it belongs to the service.
    pub async fn find_of(cx: &Cx, service_id: u64, id: u64) -> Result<Option<Domain>> {
        let domain = Domain::filter_by_id(id).first().exec(&mut db(cx)).await?;
        Ok(domain.filter(|domain| domain.service_id == service_id))
    }

    /// Adds a domain to the service. Returns what is wrong instead, if a field is
    /// invalid or another domain already takes the address.
    pub async fn add(cx: &Cx, service: &Service, new: &NewDomain<'_>) -> Result<Result<Domain, String>> {
        Ok(Self::add_in(&db(cx), service.id, new).await?)
    }

    /// Gives the service a domain at `address` through sslip.io, over HTTPS, such as
    /// `shop-api.203-0-113-7.sslip.io`, so the app opens without a domain of the
    /// user's. Returns what is wrong instead, if another domain already takes it.
    pub async fn add_generated(
        db: &Db,
        service: &Service,
        address: Ipv4Addr,
    ) -> toasty::Result<Result<Domain, String>> {
        let host = generated_host(&service.app_name, address);
        let new = NewDomain {
            host: &host,
            path: "",
            strip_path: false,
            https: true,
            redirect_www: false,
        };
        Self::add_in(db, service.id, &new).await
    }

    async fn add_in(db: &Db, service_id: u64, new: &NewDomain<'_>) -> toasty::Result<Result<Domain, String>> {
        let others = Self::all_in(db).await?;
        let panel = PanelDomain::get_in(db).await?;
        let checked = match check(new, &others, panel.as_ref().map(|panel| panel.host.as_str())) {
            Ok(checked) => checked,
            Err(problem) => return Ok(Err(problem)),
        };
        let domain = toasty::create!(Domain {
            service_id,
            host: checked.host,
            path: checked.path,
            strip_path: checked.strip_path,
            https: new.https,
            redirect_www: checked.redirect_www,
            created_at: now(),
        })
        .exec(&mut db.clone())
        .await?;
        Ok(Ok(domain))
    }

    /// Whether the service has any domain.
    pub async fn any_of_service(db: &Db, service_id: u64) -> toasty::Result<bool> {
        let first = Domain::filter_by_service_id(service_id)
            .first()
            .exec(&mut db.clone())
            .await?;
        Ok(first.is_some())
    }

    /// Changes the domain. Returns what is wrong instead, as [`add`](Self::add) does.
    pub async fn edit(&mut self, cx: &Cx, new: &NewDomain<'_>) -> Result<Result<(), String>> {
        let others: Vec<_> = Self::all_in(&db(cx))
            .await?
            .into_iter()
            .filter(|other| other.id != self.id)
            .collect();
        let panel = PanelDomain::get_in(&db(cx)).await?;
        let checked = match check(new, &others, panel.as_ref().map(|panel| panel.host.as_str())) {
            Ok(checked) => checked,
            Err(problem) => return Ok(Err(problem)),
        };
        self.update()
            .host(checked.host)
            .path(checked.path)
            .strip_path(checked.strip_path)
            .https(new.https)
            .redirect_www(checked.redirect_www)
            .exec(&mut db(cx))
            .await?;
        Ok(Ok(()))
    }

    /// Removes the domain.
    pub async fn remove(self, cx: &Cx) -> Result<()> {
        Domain::filter_by_id(self.id).delete().exec(&mut db(cx)).await?;
        Ok(())
    }

    /// Removes every domain of a service, before the service is deleted.
    pub async fn remove_all(db: &Db, service_id: u64) -> toasty::Result<()> {
        Domain::filter_by_service_id(service_id)
            .delete()
            .exec(&mut db.clone())
            .await
    }

    /// The address the domain opens, such as `https://shop.example.com/api`.
    pub fn url(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://{}{}", self.host, self.path.as_deref().unwrap_or(""))
    }

    /// The host that sends its visitors to this one, if the domain redirects www.
    pub fn www(&self) -> Option<String> {
        self.redirect_www.then(|| format!("www.{}", self.host))
    }

    /// Whether oxploy made the domain up, at the server's address through sslip.io.
    pub fn is_generated(&self) -> bool {
        self.host.ends_with(&format!(".{GENERATED_DOMAIN}"))
    }

    /// Of a service's `domains`, oldest first, the one its Open button opens: the one
    /// with the ID the user chose, or else the first of the user's own, or else the
    /// first.
    pub fn default_of(domains: &[Domain], chosen: Option<u64>) -> Option<&Domain> {
        chosen
            .and_then(|id| domains.iter().find(|domain| domain.id == id))
            .or_else(|| domains.iter().find(|domain| !domain.is_generated()))
            .or_else(|| domains.first())
    }
}

impl PanelDomain {
    /// Returns the panel's domain, if it has one.
    pub async fn get_in(db: &Db) -> toasty::Result<Option<PanelDomain>> {
        PanelDomain::all().first().exec(&mut db.clone()).await
    }

    /// Gives the panel `host`, in place of the domain it had. Returns what is wrong
    /// instead, if it is not a hostname or an app already answers at it.
    pub async fn set(cx: &Cx, host: &str) -> Result<Result<PanelDomain, String>> {
        let Some(host) = normalize_host(host) else {
            return Ok(Err(format!("{} is not a hostname, such as panel.example.com", host.trim())));
        };
        let apps = Domain::all_in(&db(cx)).await?;
        if apps
            .iter()
            .any(|app| app.host == host || app.www().as_deref() == Some(host.as_str()))
        {
            return Ok(Err(format!("{host} already reaches an app")));
        }
        Self::remove(cx).await?;
        let domain = toasty::create!(PanelDomain {
            host,
            created_at: now(),
        })
        .exec(&mut db(cx))
        .await?;
        Ok(Ok(domain))
    }

    /// Takes the panel's domain away.
    pub async fn remove(cx: &Cx) -> Result<()> {
        PanelDomain::all().delete().exec(&mut db(cx)).await?;
        Ok(())
    }

    /// The address the panel opens at, such as `https://panel.example.com`.
    pub fn url(&self) -> String {
        format!("https://{}", self.host)
    }
}

/// A domain's fields as they are stored.
#[derive(Debug, PartialEq, Eq)]
struct Checked {
    host: String,
    path: Option<String>,
    strip_path: bool,
    redirect_www: bool,
}

/// Checks a domain against the others on the server and the panel's, and returns its
/// fields as they are stored. Stripping applies only with a path, and redirecting www
/// only without one, as the form shows them.
fn check(new: &NewDomain<'_>, others: &[Domain], panel: Option<&str>) -> Result<Checked, String> {
    let host = normalize_host(new.host)
        .ok_or_else(|| format!("{} is not a hostname, such as shop.example.com", new.host.trim()))?;
    let path = normalize_path(new.path)
        .ok_or_else(|| format!("{} is not a path, such as /api", new.path.trim()))?;
    let redirect_www = new.redirect_www && path.is_none();
    if redirect_www && host.starts_with("www.") {
        return Err(format!("{host} already starts with www."));
    }
    let www = format!("www.{host}");
    if let Some(panel) = panel
        && (host == panel || (redirect_www && www == panel))
    {
        return Err(format!("{panel} is the panel's domain"));
    }
    for other in others {
        if other.host == host && other.path == path {
            return Err(format!("{} is already a domain", other.url()));
        }
        // HTTP and HTTPS are told apart per host, not per path.
        if other.host == host && other.https != new.https {
            let scheme = if other.https { "HTTPS" } else { "plain HTTP" };
            return Err(format!("{host} already serves {scheme}; use the same here"));
        }
        if redirect_www && other.host == www {
            return Err(format!("{www} is a domain of its own"));
        }
        if other.www().as_deref() == Some(host.as_str()) {
            return Err(format!("{host} already redirects to {}", other.host));
        }
    }
    Ok(Checked {
        strip_path: new.strip_path && path.is_some(),
        host,
        path,
        redirect_www,
    })
}

/// Returns the hostname in lowercase, without a scheme or a slash pasted with it, or
/// `None` if it is not one. An IP address is not a hostname.
fn normalize_host(host: &str) -> Option<String> {
    let host = host.trim().to_ascii_lowercase();
    let bare = host
        .strip_prefix("https://")
        .or_else(|| host.strip_prefix("http://"))
        .unwrap_or(&host);
    let bare = bare.strip_suffix('/').unwrap_or(bare);
    let labels: Vec<_> = bare.split('.').collect();
    let valid = bare.len() <= MAX_HOST_LEN
        && labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && labels
            .last()
            .is_some_and(|top| top.chars().any(|c| c.is_ascii_alphabetic()));
    valid.then(|| bare.to_owned())
}

/// Returns the path with one leading slash and none at the end, `None` inside for the
/// whole host, or `None` if it is not a plain path.
fn normalize_path(path: &str) -> Option<Option<String>> {
    let path = path.trim().trim_end_matches('/');
    if path.is_empty() {
        return Some(None);
    }
    let path = format!("/{}", path.trim_start_matches('/'));
    let valid = path.len() <= MAX_PATH_LEN
        && path[1..].split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
        });
    valid.then_some(Some(path))
}

/// Where a domain's name points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// To this machine.
    Here,
    /// To other addresses.
    Elsewhere(Vec<IpAddr>),
    /// Nowhere: the name has no address, or the lookup failed.
    Nowhere,
}

/// Looks up `host` and compares its addresses with this machine's. Names under
/// `localhost` always point here, as browsers treat them.
pub async fn resolve(host: &str) -> Resolution {
    if host == "localhost" || host.ends_with(".localhost") {
        return Resolution::Here;
    }
    let lookup = tokio::time::timeout(LOOKUP_TIMEOUT, tokio::net::lookup_host((host, 80))).await;
    let mut found: Vec<IpAddr> = match lookup {
        Ok(Ok(addresses)) => addresses.map(|address| address.ip()).collect(),
        _ => return Resolution::Nowhere,
    };
    found.sort();
    found.dedup();
    let here: HashSet<IpAddr> = machine_addresses().into_iter().collect();
    if found.is_empty() {
        Resolution::Nowhere
    } else if found.iter().any(|address| here.contains(address)) {
        Resolution::Here
    } else {
        Resolution::Elsewhere(found)
    }
}

/// The addresses of this machine's network interfaces.
pub fn machine_addresses() -> Vec<IpAddr> {
    let Ok(networks) = Networks::new_with_refreshed_list() else {
        return Vec::new();
    };
    networks
        .values()
        .flat_map(|network| network.ip_networks().iter().map(|ip| ip.addr))
        .collect()
}

/// This machine's public IPv4 addresses, which the internet reaches it at, unless it
/// sits behind NAT.
pub fn public_ipv4_addresses() -> Vec<IpAddr> {
    let mut found: Vec<IpAddr> = machine_addresses()
        .into_iter()
        .filter(|address| address.is_ipv4() && is_reachable(address))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// Whether the internet can reach `address`: a public one, not loopback, link-local,
/// unspecified, broadcast or kept for documentation.
pub fn is_reachable(address: &IpAddr) -> bool {
    let reserved = match address {
        IpAddr::V4(v4) => v4.is_broadcast() || v4.is_documentation() || v4.octets()[0] == 0,
        IpAddr::V6(_) => false,
    };
    is_public(address)
        && !reserved
        && !address.is_loopback()
        && !address.is_unspecified()
        && !is_link_local(address)
}

/// The address a generated domain points to: the first of the server's public
/// addresses that is an IPv4 one the internet reaches, so Let's Encrypt can certify
/// the domain. `None` when there is none, as on a laptop or behind NAT without
/// `OXPLOY_PUBLIC_IP`.
pub fn generated_address(public_ips: &[IpAddr]) -> Option<Ipv4Addr> {
    public_ips.iter().find_map(|address| match address {
        IpAddr::V4(v4) if is_reachable(address) => Some(*v4),
        _ => None,
    })
}

/// The hostname of the app's generated domain at `address`, such as
/// `shop-api.203-0-113-7.sslip.io`. sslip.io answers any name under it with the
/// address it carries, so it points to the server without a DNS record. App names
/// are already valid DNS labels.
pub fn generated_host(app_name: &str, address: Ipv4Addr) -> String {
    let [a, b, c, d] = address.octets();
    format!("{app_name}.{a}-{b}-{c}-{d}.{GENERATED_DOMAIN}")
}

/// The addresses a domain should point to: this machine's public ones, or the others
/// when it has none, as on a local network.
pub fn addresses_to_point_at() -> Vec<IpAddr> {
    let mut usable: Vec<IpAddr> = machine_addresses()
        .into_iter()
        .filter(|address| !address.is_loopback() && !address.is_unspecified() && !is_link_local(address))
        .collect();
    usable.sort();
    usable.dedup();
    let public: Vec<IpAddr> = usable.iter().copied().filter(is_public).collect();
    if public.is_empty() { usable } else { public }
}

fn is_link_local(address: &IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unicast_link_local(),
    }
}

/// Whether the address can be reached from the internet: not private, nor shared
/// carrier space, nor a unique local IPv6 address.
fn is_public(address: &IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            let [first, second, ..] = v4.octets();
            !v4.is_private() && !(first == 100 && (64..128).contains(&second))
        }
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) != 0xfc00,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(id: u64, host: &str) -> Domain {
        Domain {
            id,
            service_id: 1,
            host: host.to_owned(),
            path: None,
            strip_path: false,
            https: true,
            redirect_www: false,
            created_at: 0,
        }
    }

    #[test]
    fn open_prefers_the_chosen_domain_then_the_users_own() {
        let domains = [
            stored(1, "shop-api.203-0-113-7.sslip.io"),
            stored(2, "shop.example.com"),
            stored(3, "api.example.com"),
        ];
        assert_eq!(Domain::default_of(&domains, None).map(|domain| domain.id), Some(2));
        assert_eq!(Domain::default_of(&domains, Some(3)).map(|domain| domain.id), Some(3));
        assert_eq!(Domain::default_of(&domains, Some(1)).map(|domain| domain.id), Some(1));
        // A chosen domain since deleted falls back.
        assert_eq!(Domain::default_of(&domains, Some(9)).map(|domain| domain.id), Some(2));
        assert_eq!(Domain::default_of(&domains[..1], None).map(|domain| domain.id), Some(1));
        assert!(Domain::default_of(&[], Some(1)).is_none());
    }

    fn new<'a>(host: &'a str, path: &'a str) -> NewDomain<'a> {
        NewDomain {
            host,
            path,
            strip_path: false,
            https: true,
            redirect_www: false,
        }
    }

    fn domain(host: &str, path: Option<&str>, https: bool, redirect_www: bool) -> Domain {
        Domain {
            id: 1,
            service_id: 1,
            host: host.to_owned(),
            path: path.map(str::to_owned),
            strip_path: false,
            https,
            redirect_www,
            created_at: 0,
        }
    }

    #[test]
    fn only_public_addresses_are_reachable() {
        for address in ["203.0.114.7", "8.8.8.8", "2606:4700::1"] {
            assert!(is_reachable(&address.parse().unwrap()), "{address}");
        }
        for address in ["127.0.0.1", "10.0.0.5", "192.168.1.2", "100.64.0.1", "169.254.1.1", "203.0.113.7", "0.0.0.0", "fd00::1", "::1"] {
            assert!(!is_reachable(&address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn generated_domains_carry_the_public_ipv4_address() {
        let addresses: Vec<IpAddr> = ["10.0.0.5", "2606:4700::1", "203.0.114.7", "8.8.8.8"]
            .iter()
            .map(|address| address.parse().unwrap())
            .collect();
        let address = generated_address(&addresses).unwrap();
        assert_eq!(address, Ipv4Addr::new(203, 0, 114, 7));
        let host = generated_host("shop-api", address);
        assert_eq!(host, "shop-api.203-0-114-7.sslip.io");
        assert_eq!(normalize_host(&host), Some(host.clone()));
        assert_eq!(generated_address(&["10.0.0.5".parse().unwrap(), "::1".parse().unwrap()]), None);
        assert_eq!(generated_address(&[]), None);
    }

    #[test]
    fn hosts_are_names_in_lowercase() {
        assert_eq!(normalize_host(" Shop.Example.COM "), Some("shop.example.com".to_owned()));
        assert_eq!(normalize_host("https://shop.example.com/"), Some("shop.example.com".to_owned()));
        assert_eq!(normalize_host("shop.localhost"), Some("shop.localhost".to_owned()));
        for host in ["shop", "1.2.3.4", "-a.com", "a..com", "a.com/api", "café.com", ""] {
            assert_eq!(normalize_host(host), None, "{host}");
        }
    }

    #[test]
    fn paths_get_one_leading_slash() {
        assert_eq!(normalize_path(""), Some(None));
        assert_eq!(normalize_path("/"), Some(None));
        assert_eq!(normalize_path("api/"), Some(Some("/api".to_owned())));
        assert_eq!(normalize_path("/api/v1"), Some(Some("/api/v1".to_owned())));
        for path in ["/a//b", "/../etc", "/a?b", "/a b"] {
            assert_eq!(normalize_path(path), None, "{path}");
        }
    }

    #[test]
    fn addresses_are_taken_once() {
        let others = [
            domain("shop.com", None, true, true),
            domain("api.com", Some("/v1"), false, false),
        ];
        assert!(check(&new("shop.com", "/"), &others, None).is_err());
        assert!(check(&new("www.shop.com", ""), &others, None).is_err());
        assert!(check(&new("api.com", "/v2"), &others, None).is_err(), "api.com is plain HTTP");
        assert_eq!(
            check(&NewDomain { https: false, ..new("api.com", "/v2") }, &others, None),
            Ok(Checked {
                host: "api.com".to_owned(),
                path: Some("/v2".to_owned()),
                strip_path: false,
                redirect_www: false,
            })
        );
        assert!(check(&new("panel.com", ""), &[], Some("panel.com")).is_err());
        let www = NewDomain { redirect_www: true, ..new("panel.com", "") };
        assert!(check(&www, &[], Some("www.panel.com")).is_err());
        let with_path = NewDomain { redirect_www: true, ..new("shop.com", "/api") };
        assert_eq!(check(&with_path, &[], None).map(|checked| checked.redirect_www), Ok(false));
    }
}
