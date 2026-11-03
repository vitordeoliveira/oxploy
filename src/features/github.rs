//! GitHub Apps, which let oxploy read an organization's repositories, and the GitHub
//! API calls made with them.
//!
//! Each app is registered from a manifest: the user confirms it on GitHub, which hands
//! the app's credentials straight back to oxploy. The app is private, so it can only be
//! installed on the account that owns it, and has at most one installation.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use topcoat::{Result, context::Cx};

use super::project::now;
use crate::db::db;

/// A GitHub App registered for an organization.
#[derive(Debug, toasty::Model)]
pub struct GithubApp {
    #[key]
    #[auto]
    pub id: u64,

    #[index]
    pub organization_id: u64,

    /// GitHub's ID for the app.
    pub app_id: u64,

    pub name: String,

    /// The app's page on GitHub, such as `https://github.com/apps/oxploy-2026-11-02-k3f9qz`.
    pub html_url: String,

    /// The PEM private key oxploy signs its requests to GitHub with.
    pub private_key: String,

    pub client_id: String,

    pub client_secret: String,

    pub webhook_secret: Option<String>,

    /// The app's installation on the account that owns it, once installed.
    pub installation_id: Option<u64>,

    /// The GitHub account the app is installed on.
    pub account: Option<String>,

    /// When the app was registered, in seconds since the Unix epoch.
    pub created_at: i64,
}

/// A repository the app can read.
#[derive(Clone, Debug, Deserialize)]
pub struct Repository {
    /// The owner and name, such as `rust-lang/cargo`.
    pub full_name: String,
    pub private: bool,
}

/// The branches of a repository.
#[derive(Clone)]
pub struct Branches {
    pub default: String,
    pub names: Vec<String>,
}

/// The account that will own a new app.
pub enum Owner<'a> {
    /// The personal account of whoever confirms the app on GitHub.
    Personal,
    /// A GitHub organization, by its login.
    Organization(&'a str),
}

/// A registration ready to send to GitHub: post `manifest` to `action` from the
/// browser.
pub struct Registration {
    pub action: String,
    pub manifest: String,
}

/// Why a registration that came back from GitHub was not saved.
#[derive(Debug)]
pub enum RegistrationError {
    /// The registration was not started from this organization, or took too long.
    Expired,
    Github(GithubError),
}

/// A request to GitHub that did not succeed.
#[derive(Debug)]
pub enum GithubError {
    /// The app has no installation to act through.
    NotInstalled,
    /// The request did not reach GitHub, or no answer came back.
    Unreachable(String),
    /// GitHub refused the request.
    Rejected { status: u16, message: String },
    /// The answer, or the app's own credentials, could not be used.
    Unexpected(String),
}

impl fmt::Display for GithubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled => f.write_str("The GitHub App is not installed yet"),
            Self::Unreachable(error) => write!(f, "GitHub could not be reached: {error}"),
            Self::Rejected { status, message } => write!(f, "GitHub answered {status}: {message}"),
            Self::Unexpected(error) => write!(f, "GitHub sent an unexpected answer: {error}"),
        }
    }
}

impl std::error::Error for GithubError {}

/// How long the user may take to confirm a new app on GitHub. GitHub allows an hour for
/// the whole registration.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Registrations sent to GitHub and not back yet, by their `state`, with the
/// organization that started each one.
static PENDING: LazyLock<Mutex<HashMap<String, (u64, Instant)>>> =
    LazyLock::new(Default::default);

impl GithubApp {
    /// Returns an organization's apps, oldest first.
    pub async fn in_organization(cx: &Cx, organization_id: u64) -> Result<Vec<GithubApp>> {
        let apps = GithubApp::filter_by_organization_id(organization_id)
            .order_by(GithubApp::fields().id().asc())
            .exec(&mut db(cx))
            .await?;
        Ok(apps)
    }

    /// Returns the app with this ID if it belongs to the organization.
    pub async fn find_in(cx: &Cx, organization_id: u64, id: u64) -> Result<Option<GithubApp>> {
        let app = GithubApp::filter_by_id(id).first().exec(&mut db(cx)).await?;
        Ok(app.filter(|app| app.organization_id == organization_id))
    }

    /// Prepares the registration of a new app for an organization. GitHub sends the
    /// browser to `redirect_url` once the user confirms the app, and to `setup_url` once
    /// they install it. Returns `None` if the organization login is not a valid one.
    pub fn start_registration(
        organization_id: u64,
        owner: &Owner<'_>,
        homepage: &str,
        redirect_url: &str,
        setup_url: &str,
    ) -> Option<Registration> {
        let page = match owner {
            Owner::Personal => "https://github.com/settings/apps/new".to_owned(),
            Owner::Organization(login) if is_login(login) => {
                format!("https://github.com/organizations/{login}/settings/apps/new")
            }
            Owner::Organization(_) => return None,
        };
        let state = random_hex(16);
        {
            let mut pending = PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            pending.retain(|_, (_, started)| started.elapsed() < REGISTRATION_TIMEOUT);
            pending.insert(state.clone(), (organization_id, Instant::now()));
        }

        // Only read access to code. Webhooks come with automatic deploys, which need
        // the panel to be reachable from GitHub.
        let manifest = serde_json::json!({
            "name": format!("oxploy-{}-{}", jiff::Zoned::now().date(), random_name(6)),
            "url": homepage,
            "redirect_url": redirect_url,
            "setup_url": setup_url,
            "setup_on_update": true,
            "description": "Builds and deploys this account's Rust apps with oxploy.",
            "public": false,
            "default_permissions": { "contents": "read", "metadata": "read" },
        });
        Some(Registration {
            action: format!("{page}?state={state}"),
            manifest: manifest.to_string(),
        })
    }

    /// Saves the app GitHub registered, from the `state` and `code` it sent the browser
    /// back with.
    pub async fn finish_registration(
        cx: &Cx,
        organization_id: u64,
        state: &str,
        code: &str,
    ) -> Result<Result<GithubApp, RegistrationError>> {
        let started = PENDING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(state);
        let expected = started.is_some_and(|(organization, started)| {
            organization == organization_id && started.elapsed() < REGISTRATION_TIMEOUT
        });
        if !expected || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok(Err(RegistrationError::Expired));
        }

        #[derive(Deserialize)]
        struct Conversion {
            id: u64,
            name: String,
            html_url: String,
            client_id: String,
            client_secret: String,
            webhook_secret: Option<String>,
            pem: String,
        }
        let path = format!("/app-manifests/{code}/conversions");
        let app: Conversion = match api(Method::Post, path, None).await {
            Ok(app) => app,
            Err(error) => return Ok(Err(RegistrationError::Github(error))),
        };
        let app = toasty::create!(GithubApp {
            organization_id,
            app_id: app.id,
            name: app.name,
            html_url: app.html_url,
            private_key: app.pem,
            client_id: app.client_id,
            client_secret: app.client_secret,
            webhook_secret: app.webhook_secret,
            installation_id: None,
            account: None,
            created_at: now(),
        })
        .exec(&mut db(cx))
        .await?;
        Ok(Ok(app))
    }

    /// The GitHub page that installs the app.
    pub fn install_url(&self) -> String {
        format!("{}/installations/new", self.html_url)
    }

    /// Asks GitHub where the app is installed and records it.
    pub async fn refresh_installation(&mut self, cx: &Cx) -> Result<Result<(), GithubError>> {
        #[derive(Deserialize)]
        struct Installation {
            id: u64,
            account: Option<Account>,
        }
        #[derive(Deserialize)]
        struct Account {
            login: String,
        }
        let jwt = match self.jwt() {
            Ok(jwt) => jwt,
            Err(error) => return Ok(Err(error)),
        };
        let installations: Vec<Installation> =
            match api(Method::Get, "/app/installations".to_owned(), Some(jwt)).await {
                Ok(installations) => installations,
                Err(error) => return Ok(Err(error)),
            };
        let installation = installations.into_iter().next();
        let installation_id = installation.as_ref().map(|installation| installation.id);
        if installation_id != self.installation_id {
            let account = installation
                .and_then(|installation| installation.account)
                .map(|account| account.login);
            self.update()
                .installation_id(installation_id)
                .account(account)
                .exec(&mut db(cx))
                .await?;
        }
        Ok(Ok(()))
    }

    /// Returns the repositories the installation can read, sorted by name. The list is
    /// reused for [`LIST_REUSE`].
    pub async fn repositories(&self) -> Result<Vec<Repository>, GithubError> {
        #[derive(Deserialize)]
        struct Page {
            repositories: Vec<Repository>,
        }
        static LISTED: LazyLock<Kept<u64, Vec<Repository>>> = LazyLock::new(Default::default);

        let installation = self.installation_id.ok_or(GithubError::NotInstalled)?;
        if let Some(repositories) = recent(&LISTED, &installation) {
            return Ok(repositories);
        }
        let token = self.installation_token().await?;
        let mut repositories = Vec::new();
        for page in 1..=MAX_PAGES {
            let path = format!("/installation/repositories?per_page={PER_PAGE}&page={page}");
            let Page { repositories: batch } = api(Method::Get, path, Some(token.clone())).await?;
            let last = batch.len() < PER_PAGE;
            repositories.extend(batch);
            if last {
                break;
            }
        }
        repositories.sort_by_key(|repository| repository.full_name.to_lowercase());
        remember(&LISTED, installation, repositories.clone());
        Ok(repositories)
    }

    /// Returns the branches of a repository the installation can read. They are reused
    /// for [`LIST_REUSE`].
    pub async fn branches(&self, repository: &str) -> Result<Branches, GithubError> {
        #[derive(Deserialize)]
        struct Details {
            default_branch: String,
        }
        #[derive(Deserialize)]
        struct Branch {
            name: String,
        }
        static LISTED: LazyLock<Kept<(u64, String), Branches>> = LazyLock::new(Default::default);

        if !is_repository_name(repository) {
            return Err(GithubError::Rejected {
                status: 404,
                message: "Not Found".to_owned(),
            });
        }
        let installation = self.installation_id.ok_or(GithubError::NotInstalled)?;
        let key = (installation, repository.to_owned());
        if let Some(branches) = recent(&LISTED, &key) {
            return Ok(branches);
        }
        let token = self.installation_token().await?;
        let details: Details =
            api(Method::Get, format!("/repos/{repository}"), Some(token.clone())).await?;
        let mut names = Vec::new();
        for page in 1..=MAX_PAGES {
            let path = format!("/repos/{repository}/branches?per_page={PER_PAGE}&page={page}");
            let batch: Vec<Branch> = api(Method::Get, path, Some(token.clone())).await?;
            let last = batch.len() < PER_PAGE;
            names.extend(batch.into_iter().map(|branch| branch.name));
            if last {
                break;
            }
        }
        let branches = Branches {
            default: details.default_branch,
            names,
        };
        remember(&LISTED, key, branches.clone());
        Ok(branches)
    }

    /// Deletes the app from oxploy. It stays registered on GitHub until the user
    /// deletes it there.
    pub async fn remove(self, cx: &Cx) -> Result<()> {
        GithubApp::filter_by_id(self.id)
            .delete()
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }

    /// Deletes every app of an organization.
    pub async fn remove_all_in(cx: &Cx, organization_id: u64) -> Result<()> {
        GithubApp::filter_by_organization_id(organization_id)
            .delete()
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }

    /// Returns a short-lived token that authenticates as the app itself.
    fn jwt(&self) -> Result<String, GithubError> {
        #[derive(Serialize)]
        struct Claims {
            iat: i64,
            exp: i64,
            iss: String,
        }
        // Backdated a minute against clock drift; GitHub accepts at most ten minutes.
        let now = now();
        let claims = Claims {
            iat: now - 60,
            exp: now + 9 * 60,
            iss: self.app_id.to_string(),
        };
        let key = EncodingKey::from_rsa_pem(self.private_key.as_bytes())
            .map_err(|error| GithubError::Unexpected(format!("the app's private key: {error}")))?;
        jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key)
            .map_err(|error| GithubError::Unexpected(format!("signing a token: {error}")))
    }

    /// Returns a token that acts through the app's installation, such as to fetch its
    /// repositories with git. GitHub issues them for an hour, so each one is reused for
    /// most of that.
    pub async fn installation_token(&self) -> Result<String, GithubError> {
        const REUSE: Duration = Duration::from_secs(50 * 60);
        static TOKENS: LazyLock<Mutex<HashMap<u64, (String, Instant)>>> =
            LazyLock::new(Default::default);

        let installation = self.installation_id.ok_or(GithubError::NotInstalled)?;
        if let Some((token, issued)) = TOKENS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&installation)
            && issued.elapsed() < REUSE
        {
            return Ok(token.clone());
        }

        #[derive(Deserialize)]
        struct Issued {
            token: String,
        }
        let path = format!("/app/installations/{installation}/access_tokens");
        let Issued { token } = api(Method::Post, path, Some(self.jwt()?)).await?;
        TOKENS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(installation, (token.clone(), Instant::now()));
        Ok(token)
    }
}

/// Returns whether `name` has the shape of a repository's full name, `owner/name`.
pub fn is_repository_name(name: &str) -> bool {
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.len() <= 100
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(name.split_once('/'), Some((owner, repository)) if is_login(owner) && valid_part(repository))
}

/// Returns whether `login` has the shape of a GitHub user or organization login.
fn is_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && !login.starts_with('-')
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// How long lists of repositories and branches are reused, so moving around the panel
/// does not ask GitHub each time. New ones show up after this long.
const LIST_REUSE: Duration = Duration::from_secs(60);

/// Values read from GitHub, each with when it was read.
type Kept<K, V> = Mutex<HashMap<K, (Instant, V)>>;

/// Returns the value kept for `key`, if it is younger than [`LIST_REUSE`].
fn recent<K: Eq + Hash, V: Clone>(kept: &Kept<K, V>, key: &K) -> Option<V> {
    let kept = kept.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (at, value) = kept.get(key)?;
    (at.elapsed() < LIST_REUSE).then(|| value.clone())
}

/// Keeps `value` for `key`, dropping values that are too old to use.
fn remember<K: Eq + Hash, V>(kept: &Kept<K, V>, key: K, value: V) {
    let mut kept = kept.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    kept.retain(|_, (at, _)| at.elapsed() < LIST_REUSE);
    kept.insert(key, (Instant::now(), value));
}

/// How many results GitHub returns per page at most.
const PER_PAGE: usize = 100;

/// How many pages of repositories or branches are read at most.
const MAX_PAGES: usize = 10;

/// Sends requests to GitHub. Statuses other than success are handled by [`api`].
static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent("oxploy")
        .build()
        .into()
});

enum Method {
    Get,
    Post,
}

/// Calls the GitHub API at `path`, authorized with `bearer`, and reads the JSON answer.
async fn api<T>(method: Method, path: String, bearer: Option<String>) -> Result<T, GithubError>
where
    T: DeserializeOwned + Send + 'static,
{
    #[derive(Deserialize)]
    struct Failure {
        message: String,
    }

    // `ureq` blocks, so the request runs off the async threads.
    let call = move || {
        let url = format!("https://api.github.com{path}");
        let response = match method {
            Method::Get => headers(AGENT.get(&url), bearer.as_deref()).call(),
            Method::Post => headers(AGENT.post(&url), bearer.as_deref()).send_empty(),
        };
        let mut response =
            response.map_err(|error| GithubError::Unreachable(error.to_string()))?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            let message = response
                .body_mut()
                .read_json::<Failure>()
                .map_or_else(|_| "no details".to_owned(), |failure| failure.message);
            return Err(GithubError::Rejected { status, message });
        }
        response
            .body_mut()
            .read_json::<T>()
            .map_err(|error| GithubError::Unexpected(error.to_string()))
    };
    tokio::task::spawn_blocking(call)
        .await
        .unwrap_or_else(|error| Err(GithubError::Unexpected(error.to_string())))
}

/// Adds the headers every GitHub API request carries.
fn headers<B>(request: ureq::RequestBuilder<B>, bearer: Option<&str>) -> ureq::RequestBuilder<B> {
    let request = request
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    match bearer {
        Some(token) => request.header("Authorization", format!("Bearer {token}")),
        None => request,
    }
}

/// Returns `bytes` random bytes as hexadecimal, for values nobody may guess.
fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0; bytes];
    getrandom::fill(&mut buffer).expect("the system provides random bytes");
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Returns `len` random lowercase letters and digits, to tell names apart.
fn random_name(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut buffer = vec![0; len];
    getrandom::fill(&mut buffer).expect("the system provides random bytes");
    buffer
        .iter()
        .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]))
        .collect()
}
