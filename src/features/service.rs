//! Services, the Rust apps a project deploys, stored in the database.

use tokio::sync::Mutex;
use topcoat::{Result, context::Cx};

use super::app_mcp::{self, McpTool};
use super::archive;
use super::caddy::Caddy;
use super::databases::{self, ManagedDatabase};
use super::deploy::Deployer;
use super::domain::Domain;
use super::environment::{self, EnvironmentError};
use super::github::is_repository_name;
use super::mcp::McpTokenTool;
use super::project::{Details, Project, now};
use crate::db::db;

/// A Rust app deployed from a repository.
#[derive(Debug, toasty::Model)]
pub struct Service {
    #[key]
    #[auto]
    pub id: u64,

    pub name: String,

    /// Names the app on the server, such as its container and the folders it builds
    /// in. Derived from the project and service names when the service is created, and
    /// fixed from then on.
    #[unique]
    pub app_name: String,

    pub description: Option<String>,

    #[index]
    pub project_id: u64,

    /// When the service was created, in seconds since the Unix epoch.
    pub created_at: i64,

    /// The GitHub App that reads the repository. It belongs to the service's
    /// organization, and may have been removed since.
    pub github_app_id: Option<u64>,

    /// The repository to build, as `owner/name`.
    pub repository: Option<String>,

    pub branch: Option<String>,

    /// The folder with the `Cargo.toml` to build, from the repository root, such as
    /// `crates/api`. `None` builds the root.
    pub build_path: Option<String>,

    /// The Rust toolchain to build with, such as `stable` or `1.85`. `None` uses the
    /// repository's `rust-toolchain.toml`, or stable when it has none.
    pub rust_version: Option<String>,

    /// The binary to build, passed to `cargo build --bin`. `None` builds the package's
    /// only binary.
    pub cargo_bin: Option<String>,

    /// The features to enable, comma separated, passed to `cargo build --features`.
    pub cargo_features: Option<String>,

    /// How the service is built. `None` is cargo, as for services made before build
    /// scripts.
    pub build_kind: Option<BuildKind>,

    /// The shell script that builds the service, when it builds with a script.
    pub build_script: Option<String>,

    /// The file the build script leaves in `OXPLOY_OUTPUT_DIR`, which oxploy starts.
    pub script_binary: Option<String>,

    /// The file of the repository's latest GitHub release to deploy, when the service
    /// deploys releases, such as `api-x86_64-linux.tar.gz`. A `*` stands for any text,
    /// such as the version.
    pub github_asset: Option<String>,

    /// The binary in the release's file, when the file is an archive, such as `api` or
    /// `bin/api`. A file that is not an archive is the binary itself.
    pub github_asset_binary: Option<String>,

    /// Whether a new GitHub release deploys the service on its own. `None` is no.
    pub deploys_new_releases: Option<bool>,

    /// A shell script that runs after each build, before the new release starts, such
    /// as one that migrates the database.
    pub after_build: Option<String>,

    /// The environment variables, written like a `.env` file, as the user wrote them.
    /// See [`environment`].
    pub environment: Option<String>,

    /// The port on 127.0.0.1 the app listens on, passed to it as `PORT`. Chosen the
    /// first time the app starts, and kept.
    pub port: Option<u64>,

    /// The deployment whose build runs: the latest one that went live.
    pub release_id: Option<u64>,

    /// When the user stopped the app, in seconds since the Unix epoch. `None` while it
    /// should run.
    pub stopped_at: Option<i64>,

    /// When oxploy decided whether the app needed a domain at the server's address, in
    /// seconds since the Unix epoch: the first time it went live while the server had
    /// a public address. It gives one only if the service had no domain then, and only
    /// that once, so a domain the user removes stays removed.
    pub domain_offered_at: Option<i64>,

    /// The domain the service's Open button opens. Without one, or once it is deleted, it
    /// opens the first domain of the user's own, or else the one oxploy gave the app.
    pub default_domain_id: Option<u64>,

    /// The path the app serves MCP at on its port, such as `/mcp`, or `None` when it
    /// serves none. See [`app_mcp`].
    pub mcp_path: Option<String>,

    /// The token oxploy gives the app in `OXPLOY_MCP_TOKEN` and calls its MCP server
    /// with. Made the first time MCP is turned on, and kept when it is turned off, so
    /// turning it on again needs no restart. An external MCP server's is the one it
    /// takes, which oxploy made or the user gave.
    pub mcp_token: Option<String>,

    /// What the service is. `None` is an app, as for services made before external MCP
    /// servers.
    pub kind: Option<ServiceKind>,

    /// Where an external MCP server answers, such as `https://api.example.com/mcp`.
    /// See [`app_mcp::external_url`].
    pub mcp_url: Option<String>,
}

/// What a service is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, toasty::Embed)]
pub enum ServiceKind {
    /// A Rust app that oxploy builds and runs.
    App,
    /// An MCP server that runs elsewhere, which oxploy only calls, at its URL.
    ExternalMcp,
}

/// Why an external MCP server's URL or token was turned down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalProblem {
    /// The name or description is not valid.
    Details,
    /// The URL is not one oxploy calls; see [`app_mcp::external_url`].
    Url,
    /// The token cannot be sent in a header; see [`app_mcp::is_token`].
    Token,
}

impl std::fmt::Display for ExternalProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Details => "The name is required, and the name and description must not be too long.",
            Self::Url => "The URL starts with https:// and names a host, such as https://api.example.com/mcp, without a user or a #.",
            Self::Token => "The token has letters, digits and symbols only, without spaces, and up to 512 of them.",
        })
    }
}

/// How a service is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, toasty::Embed)]
pub enum BuildKind {
    /// `cargo build --release`, with the options in the cargo settings.
    Cargo,
    /// A shell script the user writes.
    Script,
    /// A file of the repository's latest GitHub release, built elsewhere: the binary,
    /// or an archive with it.
    Release,
}

/// A build script, as the user wrote it.
pub struct BuildScript<'a> {
    pub script: &'a str,
    /// The file the script leaves in `OXPLOY_OUTPUT_DIR`.
    pub binary: &'a str,
}

/// The file of a GitHub release a service deploys, as the user typed it.
pub struct ReleaseBuild<'a> {
    /// The file's name, where `*` stands for any text.
    pub file: &'a str,
    /// The binary in the file, when it is an archive.
    pub binary: &'a str,
    /// Whether new releases deploy on their own.
    pub automatic: bool,
}

/// The most bytes a build script may take.
pub const MAX_SCRIPT_LEN: usize = 16 * 1024;

/// Where a service's code comes from, as the user chose it.
pub struct Source<'a> {
    pub github_app_id: u64,
    pub repository: &'a str,
    pub branch: &'a str,
    pub build_path: &'a str,
}

/// How cargo builds a service, as the user typed it.
pub struct CargoBuild<'a> {
    pub rust_version: &'a str,
    pub binary: &'a str,
    pub features: &'a str,
}

/// The most features a build may enable.
const MAX_FEATURES: usize = 32;

/// The most characters an app name may have, which keeps it a valid DNS label.
const MAX_APP_NAME_LEN: usize = 63;

/// Serializes picking app names, so two services created at once cannot get the same
/// one.
static NAMING: Mutex<()> = Mutex::const_new(());

impl Service {
    /// Returns the services of a project, oldest first.
    pub async fn in_project(cx: &Cx, project_id: u64) -> Result<Vec<Service>> {
        let services = Service::filter_by_project_id(project_id)
            .order_by(Service::fields().id().asc())
            .exec(&mut db(cx))
            .await?;
        Ok(services)
    }

    /// Returns how many services a project has.
    pub async fn count_in(cx: &Cx, project_id: u64) -> Result<u64> {
        let count = Service::filter_by_project_id(project_id)
            .count()
            .exec(&mut db(cx))
            .await?;
        Ok(count)
    }

    /// Returns the service with this ID if it belongs to the project.
    pub async fn find_in(cx: &Cx, project_id: u64, id: u64) -> Result<Option<Service>> {
        let service = Service::filter_by_id(id).first().exec(&mut db(cx)).await?;
        Ok(service.filter(|service| service.project_id == project_id))
    }

    /// Creates an app in `project`. Returns `None`, creating nothing, if the details
    /// are invalid.
    pub async fn add(cx: &Cx, project: &Project, details: &Details<'_>) -> Result<Option<Service>> {
        let Some((name, description)) = details.normalize() else {
            return Ok(None);
        };
        Self::insert(cx, project, name, description, None).await.map(Some)
    }

    /// Creates an external MCP server in `project`, which oxploy calls at `url` with
    /// `token`, or with a token it makes when there is none. Returns why not instead,
    /// creating nothing, if the details, the URL or the token are not valid.
    pub async fn add_external(
        cx: &Cx,
        project: &Project,
        details: &Details<'_>,
        url: &str,
        token: Option<&str>,
    ) -> Result<Result<Service, ExternalProblem>> {
        let Some((name, description)) = details.normalize() else {
            return Ok(Err(ExternalProblem::Details));
        };
        let (url, token) = match external_settings(url, token) {
            Ok(settings) => settings,
            Err(problem) => return Ok(Err(problem)),
        };
        let external = (url, token.unwrap_or_else(app_mcp::new_token));
        Self::insert(cx, project, name, description, Some(external)).await.map(Ok)
    }

    /// Changes where an external MCP server answers and the token it takes, keeping the
    /// token when `token` is `None`. Returns why not instead, changing nothing, if the
    /// URL or the token are not valid.
    pub async fn set_external_mcp(
        &mut self,
        cx: &Cx,
        url: &str,
        token: Option<&str>,
    ) -> Result<Result<(), ExternalProblem>> {
        let (url, token) = match external_settings(url, token) {
            Ok(settings) => settings,
            Err(problem) => return Ok(Err(problem)),
        };
        let token = token.or_else(|| self.mcp_token.clone()).unwrap_or_else(app_mcp::new_token);
        self.update()
            .mcp_url(Some(url))
            .mcp_token(Some(token))
            .exec(&mut db(cx))
            .await?;
        Ok(Ok(()))
    }

    /// Gives an external MCP server a new token oxploy made, for the server to take in
    /// place of the old one.
    pub async fn renew_mcp_token(&mut self, cx: &Cx) -> Result<()> {
        self.update()
            .mcp_token(Some(app_mcp::new_token()))
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }

    /// Whether it is an MCP server that runs elsewhere, rather than an app.
    pub fn is_external(&self) -> bool {
        self.kind == Some(ServiceKind::ExternalMcp)
    }

    /// Whether it serves MCP: an app with MCP turned on in its MCP tab, or an external
    /// MCP server.
    pub fn serves_mcp(&self) -> bool {
        if self.is_external() {
            self.mcp_url.is_some() && self.mcp_token.is_some()
        } else {
            self.mcp_token_in_use().is_some()
        }
    }

    /// Creates a service named `name`, an app or, with `external`'s URL and token, an
    /// external MCP server.
    async fn insert(
        cx: &Cx,
        project: &Project,
        name: String,
        description: Option<String>,
        external: Option<(String, String)>,
    ) -> Result<Service> {
        let _naming = NAMING.lock().await;
        let app_name = unused_app_name(cx, &project.name, &name).await?;
        let service = toasty::create!(Service {
            name,
            app_name,
            description,
            project_id: project.id,
            created_at: now(),
            github_app_id: None,
            repository: None,
            branch: None,
            build_path: None,
            rust_version: None,
            cargo_bin: None,
            cargo_features: None,
            build_kind: None,
            build_script: None,
            script_binary: None,
            github_asset: None,
            github_asset_binary: None,
            deploys_new_releases: None,
            after_build: None,
            environment: None,
            port: None,
            release_id: None,
            stopped_at: None,
            domain_offered_at: None,
            default_domain_id: None,
            mcp_path: None,
            mcp_token: external.as_ref().map(|(_, token)| token.clone()),
            kind: Some(if external.is_some() { ServiceKind::ExternalMcp } else { ServiceKind::App }),
            mcp_url: external.map(|(url, _)| url),
        })
        .exec(&mut db(cx))
        .await?;
        Ok(service)
    }

    /// Replaces the name and description. The app name stays. Returns `false`, changing
    /// nothing, if the details are invalid.
    pub async fn edit(&mut self, cx: &Cx, details: &Details<'_>) -> Result<bool> {
        let Some((name, description)) = details.normalize() else {
            return Ok(false);
        };
        self.update()
            .name(name)
            .description(description)
            .exec(&mut db(cx))
            .await?;
        Ok(true)
    }

    /// Sets where the code comes from. Returns `false`, changing nothing, if a value is
    /// invalid. The caller checks that the app belongs to the service's organization.
    pub async fn set_source(&mut self, cx: &Cx, source: &Source<'_>) -> Result<bool> {
        let repository = source.repository.trim();
        let branch = source.branch.trim();
        let Some(build_path) = normalize_build_path(source.build_path) else {
            return Ok(false);
        };
        if !is_repository_name(repository) || !is_branch_name(branch) {
            return Ok(false);
        }
        self.update()
            .github_app_id(Some(source.github_app_id))
            .repository(Some(repository.to_owned()))
            .branch(Some(branch.to_owned()))
            .build_path(build_path)
            .exec(&mut db(cx))
            .await?;
        Ok(true)
    }

    /// Sets how cargo builds the service. Returns `false`, changing nothing, if a value
    /// is invalid. Empty values mean the defaults.
    pub async fn set_build(&mut self, cx: &Cx, build: &CargoBuild<'_>) -> Result<bool> {
        let rust_version = build.rust_version.trim();
        let binary = build.binary.trim();
        let Some(features) = normalize_features(build.features) else {
            return Ok(false);
        };
        let valid = (rust_version.is_empty() || is_rust_version(rust_version))
            && (binary.is_empty() || is_target_name(binary));
        if !valid {
            return Ok(false);
        }
        let optional = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        self.update()
            .build_kind(Some(BuildKind::Cargo))
            .rust_version(optional(rust_version))
            .cargo_bin(optional(binary))
            .cargo_features(features)
            .exec(&mut db(cx))
            .await?;
        Ok(true)
    }

    /// Replaces the environment variables, keeping them as written. Returns the problem,
    /// changing nothing, if a line is not a variable oxploy can pass on.
    pub async fn set_environment(
        &mut self,
        cx: &Cx,
        text: &str,
    ) -> Result<Result<(), EnvironmentError>> {
        let text = text.replace("\r\n", "\n");
        let text = text.trim();
        let databases = ManagedDatabase::of_service(cx, self.id).await?;
        if let Err(error) = environment::parse(text, &databases::set_elsewhere(&databases)) {
            return Ok(Err(error));
        }
        let text = (!text.is_empty()).then(|| text.to_owned());
        self.update().environment(text).exec(&mut db(cx)).await?;
        Ok(Ok(()))
    }

    /// Makes the service build with a script. Returns `false`, changing nothing, if the
    /// script is empty or too long, or the binary is not a plain file name.
    pub async fn set_build_script(&mut self, cx: &Cx, build: &BuildScript<'_>) -> Result<bool> {
        let script = build.script.replace("\r\n", "\n");
        let script = script.trim();
        let binary = build.binary.trim();
        let valid = !script.is_empty() && script.len() <= MAX_SCRIPT_LEN && is_file_name(binary);
        if !valid {
            return Ok(false);
        }
        self.update()
            .build_kind(Some(BuildKind::Script))
            .build_script(Some(script.to_owned()))
            .script_binary(Some(binary.to_owned()))
            .exec(&mut db(cx))
            .await?;
        Ok(true)
    }

    /// Makes the service deploy a file of its repository's latest GitHub release.
    /// Returns `false`, changing nothing, if the file is not a file name, or the binary
    /// is not a path inside the archive. The binary is kept only for an archive.
    pub async fn set_release(&mut self, cx: &Cx, build: &ReleaseBuild<'_>) -> Result<bool> {
        let file = build.file.trim();
        let binary = build.binary.trim().trim_start_matches("./");
        if !is_release_file(file) {
            return Ok(false);
        }
        let binary = if archive::is_archive(file) {
            if !is_path_inside(binary) {
                return Ok(false);
            }
            Some(binary.to_owned())
        } else {
            None
        };
        self.update()
            .build_kind(Some(BuildKind::Release))
            .github_asset(Some(file.to_owned()))
            .github_asset_binary(binary)
            .deploys_new_releases(Some(build.automatic))
            .exec(&mut db(cx))
            .await?;
        Ok(true)
    }

    /// Makes the domain with this ID the one the service's Open button opens. The
    /// caller checks that it is one of the service's.
    pub async fn set_default_domain(&mut self, cx: &Cx, domain_id: u64) -> Result<()> {
        self.update()
            .default_domain_id(Some(domain_id))
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }

    /// Sets the script that runs after each build. An empty one means none. Returns
    /// `false`, changing nothing, if it is too long.
    pub async fn set_after_build(&mut self, cx: &Cx, script: &str) -> Result<bool> {
        let script = script.replace("\r\n", "\n");
        let script = script.trim();
        if script.len() > MAX_SCRIPT_LEN {
            return Ok(false);
        }
        let script = (!script.is_empty()).then(|| script.to_owned());
        self.update().after_build(script).exec(&mut db(cx)).await?;
        Ok(true)
    }

    /// Turns the app's MCP server on at `path`, or off with `None`, and has Caddy hide
    /// the path from the app's domains. Returns `false`, changing nothing, if the path
    /// cannot be one; see [`app_mcp::is_path`].
    pub async fn set_mcp(&mut self, cx: &Cx, path: Option<&str>) -> Result<bool> {
        let path = path.map(str::trim);
        if path.is_some_and(|path| !app_mcp::is_path(path)) {
            return Ok(false);
        }
        let token = match (&self.mcp_token, path) {
            (None, Some(_)) => Some(app_mcp::new_token()),
            (token, _) => token.clone(),
        };
        self.update()
            .mcp_path(path.map(str::to_owned))
            .mcp_token(token)
            .exec(&mut db(cx))
            .await?;
        Caddy::get(cx).changed();
        Ok(true)
    }

    /// The token the app's MCP server takes, while it serves one.
    pub fn mcp_token_in_use(&self) -> Option<&str> {
        self.mcp_path.as_ref().and(self.mcp_token.as_deref())
    }

    /// How the service is built.
    pub fn builds_with(&self) -> BuildKind {
        self.build_kind.unwrap_or(BuildKind::Cargo)
    }

    /// The cargo command that builds the service.
    pub fn build_command(&self) -> String {
        cargo_command(
            self.rust_version.as_deref().unwrap_or(""),
            self.cargo_bin.as_deref().unwrap_or(""),
            self.cargo_features.as_deref().unwrap_or(""),
        )
    }

    /// Stops the service and deletes it, with its deployments and files.
    pub async fn remove(self, cx: &Cx) -> Result<()> {
        Deployer::get(cx).forget(&self).await?;
        Domain::remove_all(&db(cx), self.id).await?;
        McpTool::remove_all(&db(cx), self.id).await?;
        McpTokenTool::remove_all(&db(cx), self.id).await?;
        Caddy::get(cx).changed();
        Service::filter_by_id(self.id)
            .delete()
            .exec(&mut db(cx))
            .await?;
        Ok(())
    }

    /// Deletes every service of a project, as [`remove`](Self::remove) does.
    pub async fn remove_all_in(cx: &Cx, project_id: u64) -> Result<()> {
        for service in Self::in_project(cx, project_id).await? {
            service.remove(cx).await?;
        }
        Ok(())
    }
}

/// An external MCP server's URL, as oxploy keeps it, and the token the user gave, or
/// why either is not valid. An empty token is none.
fn external_settings(url: &str, token: Option<&str>) -> Result<(String, Option<String>), ExternalProblem> {
    let url = app_mcp::external_url(url).ok_or(ExternalProblem::Url)?;
    let token = token.map(str::trim).filter(|token| !token.is_empty());
    if token.is_some_and(|token| !app_mcp::is_token(token)) {
        return Err(ExternalProblem::Token);
    }
    Ok((url, token.map(str::to_owned)))
}

/// Returns an app name no other service uses, such as `shop-api`, or `shop-api-2`
/// when that one is taken.
async fn unused_app_name(cx: &Cx, project: &str, service: &str) -> Result<String> {
    let mut base: String = [slug(project), slug(service)]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if base.is_empty() {
        base = "app".to_owned();
    }
    // Leaves room for a suffix.
    base.truncate(MAX_APP_NAME_LEN - 4);
    let base = base.trim_end_matches('-');

    let mut db = db(cx);
    for attempt in 1.. {
        let candidate = match attempt {
            1 => base.to_owned(),
            n => format!("{base}-{n}"),
        };
        let taken = Service::filter_by_app_name(&candidate)
            .first()
            .exec(&mut db)
            .await?
            .is_some();
        if !taken {
            return Ok(candidate);
        }
    }
    unreachable!("some suffix is free")
}

/// Lowercases ASCII letters and digits and joins the runs between them with dashes,
/// so `My API!` becomes `my-api`.
pub fn slug(text: &str) -> String {
    let mut slug = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// Returns the build path without its outer slashes, `None` for the repository root,
/// or no answer if it leaves the repository or is not a plain folder path.
fn normalize_build_path(path: &str) -> Option<Option<String>> {
    let path = path.trim().trim_matches('/');
    if path.is_empty() {
        return Some(None);
    }
    let valid = path.len() <= 255
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && !segment.chars().any(|c| c.is_control() || c == '\\')
        });
    valid.then(|| Some(path.to_owned()))
}

/// Returns whether `name` names a release's file: a file name, where `*` stands for
/// any text, that is not only stars.
fn is_release_file(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.chars().any(|c| c != '*')
        && !name.chars().any(|c| c == '/' || c == '\\' || c.is_control())
        && name != "."
        && name != ".."
}

/// Returns whether `path` is a file path that stays inside the folder it starts from,
/// such as `api` or `bin/api`.
fn is_path_inside(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 255
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && !segment.chars().any(|c| c.is_control() || c == '\\')
        })
}

/// Returns whether `name` matches a release file's name as the user typed it, where
/// `*` stands for any text.
pub fn release_file_matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        // No star: the whole name must match.
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// The name to save for a file of the release tagged `tag`: the file's name, with the
/// release's version in it as `*`, so that the files of the next releases match too.
/// `api-v1.2.0-linux.tar.gz` of `v1.2.0` is `api-*-linux.tar.gz`.
pub fn release_file_pattern(tag: &str, name: &str) -> String {
    let version = tag.trim_start_matches('v');
    // A short version, such as `1`, could be part of anything else in the name.
    [tag, version]
        .into_iter()
        .find(|version| version.len() >= 3 && name.contains(version))
        .map_or_else(|| name.to_owned(), |version| name.replacen(version, "*", 1))
}

/// A guess at the binary in a release's archive, from the archive's name without its
/// extension, version and platform: `api` for `api-v1.2.0-x86_64-unknown-linux-gnu.tar.gz`.
pub fn guess_binary(file: &str) -> String {
    const PLATFORM: [&str; 16] = [
        "x86_64", "amd64", "aarch64", "arm64", "armv7", "i686", "linux", "gnu", "musl",
        "unknown", "apple", "darwin", "macos", "windows", "msvc", "pc",
    ];
    let stem = file.trim_end_matches(".tar.gz").trim_end_matches(".tgz");
    let kept: Vec<&str> = stem
        .split('-')
        .filter(|part| {
            let version = part.trim_start_matches('v');
            !part.is_empty()
                && *part != "*"
                && !version.starts_with(|c: char| c.is_ascii_digit())
                && !PLATFORM.contains(part)
        })
        .collect();
    if kept.is_empty() { stem.to_owned() } else { kept.join("-") }
}

/// Returns whether `name` is a branch name git accepts, following
/// `git check-ref-format`.
fn is_branch_name(name: &str) -> bool {
    const FORBIDDEN: [char; 9] = [' ', '~', '^', ':', '?', '*', '[', '\\', '\u{7f}'];
    !name.is_empty()
        && name.len() <= 255
        && name != "@"
        && !name.starts_with('-')
        && !name.contains("..")
        && !name.contains("@{")
        && !name.ends_with('.')
        && !name.chars().any(|c| c.is_control() || FORBIDDEN.contains(&c))
        && name.split('/').all(|component| {
            !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
        })
}

/// Returns whether `version` names a Rust toolchain: a channel such as `stable`, a
/// dated `nightly-2025-01-31`, or a release such as `1.85` or `1.85.0`.
fn is_rust_version(version: &str) -> bool {
    let is_number = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
    let is_date = |date: &str| {
        let parts: Vec<_> = date.split('-').collect();
        matches!(parts.as_slice(), [year, month, day]
            if year.len() == 4 && month.len() == 2 && day.len() == 2
                && parts.iter().all(|part| is_number(part)))
    };
    match version {
        "stable" | "beta" | "nightly" => true,
        _ => {
            if let Some(date) = version
                .strip_prefix("nightly-")
                .or_else(|| version.strip_prefix("beta-"))
            {
                return is_date(date);
            }
            let parts: Vec<_> = version.split('.').collect();
            (2..=3).contains(&parts.len()) && parts.iter().all(|part| is_number(part))
        }
    }
}

/// Returns whether `name` is a plain file name, without folders, that is not hidden.
fn is_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Returns whether `name` is a valid name for a cargo target, such as a binary.
fn is_target_name(name: &str) -> bool {
    name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The cargo command a build runs, from the Build card's fields as typed. The Rust
/// version shows as rustup's `+toolchain`, which picks the same toolchain as the
/// `RUSTUP_TOOLCHAIN` the build sets, and the features as they are saved. The Build
/// card repeats this in JavaScript to update it while the user types.
pub fn cargo_command(rust_version: &str, binary: &str, features: &str) -> String {
    let mut command = "cargo".to_owned();
    if !rust_version.trim().is_empty() {
        command += &format!(" +{}", rust_version.trim());
    }
    command += " build --release";
    if !binary.trim().is_empty() {
        command += &format!(" --bin {}", binary.trim());
    }
    let features: Vec<_> = features
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|feature| !feature.is_empty())
        .collect();
    if !features.is_empty() {
        command += &format!(" --features {}", features.join(","));
    }
    command
}

/// Returns the features as a comma separated list, `None` for no features, or no
/// answer if one is not a feature name. Features may be separated by commas or spaces,
/// and may name a dependency's feature, as in `serde/derive`.
fn normalize_features(features: &str) -> Option<Option<String>> {
    let features: Vec<_> = features
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|feature| !feature.is_empty())
        .collect();
    let valid = features.len() <= MAX_FEATURES
        && features.iter().all(|feature| {
            feature.len() <= 64
                && feature
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '.' | '/' | '?'))
        });
    valid.then(|| (!features.is_empty()).then(|| features.join(",")))
}

#[cfg(test)]
mod tests {
    use topcoat::context::CxTestBuilder;

    use super::*;
    use crate::db::TestDb;

    #[tokio::test]
    async fn an_external_mcp_server_is_called_at_its_url_with_its_token() {
        let test = TestDb::new().await;
        let cx = CxTestBuilder::new().app_context(test.db.clone()).build();
        let project = Project::add(&cx, 7, &Details { name: "Inumero", description: "" })
            .await
            .unwrap()
            .unwrap();
        let details = Details { name: "API", description: "" };

        let made = Service::add_external(&cx, &project, &details, " https://api.inumero.test/mcp ", None)
            .await
            .unwrap()
            .unwrap();
        assert!(made.is_external() && made.serves_mcp());
        assert_eq!(made.app_name, "inumero-api");
        assert_eq!(made.mcp_url.as_deref(), Some("https://api.inumero.test/mcp"));
        let made_token = made.mcp_token.clone().unwrap();
        assert_eq!(made_token.len(), 64);
        // No Deployer is needed: an external server is called whether or not anything
        // runs here.
        let endpoint = app_mcp::endpoint(&cx, &made).unwrap();
        assert_eq!(endpoint.url, "https://api.inumero.test/mcp");
        assert_eq!(endpoint.token, made_token);
        assert!(endpoint.external);

        let mut pasted = Service::add_external(&cx, &project, &details, "https://mcp.example.com", Some(" sk-live-1 "))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pasted.app_name, "inumero-api-2");
        assert_eq!(pasted.mcp_token.as_deref(), Some("sk-live-1"));

        // A bad URL, token or name makes nothing.
        for (url, token, problem) in [
            ("http://api.inumero.test/mcp", None, ExternalProblem::Url),
            ("https://api.inumero.test/mcp", Some("two words"), ExternalProblem::Token),
        ] {
            let refused = Service::add_external(&cx, &project, &details, url, token).await.unwrap();
            assert_eq!(refused.unwrap_err(), problem);
        }
        let nameless = Details { name: " ", description: "" };
        let refused = Service::add_external(&cx, &project, &nameless, "https://a.test", None).await.unwrap();
        assert_eq!(refused.unwrap_err(), ExternalProblem::Details);
        assert_eq!(Service::count_in(&cx, project.id).await.unwrap(), 2);

        // Changing the URL keeps the token unless one is given.
        pasted.set_external_mcp(&cx, "https://mcp.example.com/v2", None).await.unwrap().unwrap();
        assert_eq!(pasted.mcp_url.as_deref(), Some("https://mcp.example.com/v2"));
        assert_eq!(pasted.mcp_token.as_deref(), Some("sk-live-1"));
        pasted.set_external_mcp(&cx, "https://mcp.example.com/v2", Some("sk-live-2")).await.unwrap().unwrap();
        assert_eq!(pasted.mcp_token.as_deref(), Some("sk-live-2"));
        assert_eq!(
            pasted.set_external_mcp(&cx, "https://10.0.0.1:x/mcp", None).await.unwrap(),
            Err(ExternalProblem::Url)
        );
        pasted.renew_mcp_token(&cx).await.unwrap();
        assert_ne!(pasted.mcp_token.as_deref(), Some("sk-live-2"));

        // Its tools reach the organization once turned on, as an app's do.
        McpTool::set(&cx, made.id, "list_clients", true).await.unwrap();
        let serving = app_mcp::serving(&cx, 7).await.unwrap();
        let names: Vec<_> = serving.iter().map(|app| (app.service.app_name.as_str(), app.enabled.clone())).collect();
        assert_eq!(names, [("inumero-api", vec!["list_clients".to_owned()])]);

        // An app is none of this.
        let app = Service::add(&cx, &project, &Details { name: "Site", description: "" })
            .await
            .unwrap()
            .unwrap();
        assert!(!app.is_external() && !app.serves_mcp());
    }

    #[test]
    fn build_paths_stay_inside_the_repository() {
        assert_eq!(normalize_build_path(" / "), Some(None));
        assert_eq!(normalize_build_path("/crates/api/"), Some(Some("crates/api".to_owned())));
        assert_eq!(normalize_build_path("../secrets"), None);
        assert_eq!(normalize_build_path("crates//api"), None);
        assert_eq!(normalize_build_path("crates\\api"), None);
    }

    #[test]
    fn branch_names_follow_git() {
        assert!(is_branch_name("main"));
        assert!(is_branch_name("feature/login-2"));
        assert!(!is_branch_name("-x"));
        assert!(!is_branch_name("a..b"));
        assert!(!is_branch_name("with space"));
        assert!(!is_branch_name("topic.lock"));
        assert!(!is_branch_name("a/.hidden"));
    }

    #[test]
    fn rust_versions_are_channels_or_releases() {
        for version in ["stable", "nightly", "1.85", "1.85.0", "nightly-2025-01-31"] {
            assert!(is_rust_version(version), "{version}");
        }
        for version in ["1", "1.x", "latest", "nightly-2025-1-31", "1.85.0.1"] {
            assert!(!is_rust_version(version), "{version}");
        }
    }

    #[test]
    fn features_are_joined_with_commas() {
        assert_eq!(normalize_features(""), Some(None));
        assert_eq!(
            normalize_features("postgres, tls serde/derive"),
            Some(Some("postgres,tls,serde/derive".to_owned()))
        );
        assert_eq!(normalize_features("bad;feature"), None);
    }

    #[test]
    fn cargo_commands_show_the_typed_fields() {
        assert_eq!(cargo_command("", " ", ""), "cargo build --release");
        assert_eq!(
            cargo_command(" 1.85 ", "api", "postgres, tls  serde/derive,"),
            "cargo +1.85 build --release --bin api --features postgres,tls,serde/derive"
        );
    }

    #[test]
    fn script_binaries_are_plain_file_names() {
        assert!(is_file_name("api"));
        assert!(is_file_name("shop-api.bin"));
        assert!(!is_file_name("../api"));
        assert!(!is_file_name("bin/api"));
        assert!(!is_file_name(".hidden"));
        assert!(!is_file_name(""));
    }

    #[test]
    fn app_names_are_slugs() {
        assert_eq!(slug("My API!"), "my-api");
        assert_eq!(slug("  Ação  "), "a-o");
    }

    #[test]
    fn release_files_match_with_stars() {
        assert!(release_file_matches("api-x86_64-linux.tar.gz", "api-x86_64-linux.tar.gz"));
        assert!(!release_file_matches("api-x86_64-linux.tar.gz", "api-x86_64-linux.tar.gz.sha256"));
        assert!(release_file_matches("api-*-linux.tar.gz", "api-v1.2.0-x86_64-linux.tar.gz"));
        assert!(release_file_matches("api-*", "api-"));
        assert!(release_file_matches("*.tar.gz", "api.tar.gz"));
        assert!(release_file_matches("api-*-*.tar.gz", "api-1.2-linux.tar.gz"));
        assert!(!release_file_matches("api-*-linux.tar.gz", "api-linux.tar.gz"));
        assert!(!release_file_matches("api-*.tar.gz", "web-1.0.tar.gz"));
        assert!(!release_file_matches("a*a", "a"));
    }

    #[test]
    fn release_files_and_binaries_stay_plain() {
        assert!(is_release_file("api-*-x86_64-linux.tar.gz"));
        assert!(!is_release_file("*"));
        assert!(!is_release_file("dist/api"));
        assert!(!is_release_file(""));
        assert!(is_path_inside("bin/api"));
        assert!(!is_path_inside("../api"));
        assert!(!is_path_inside("/usr/bin/api"));
        assert!(!is_path_inside("bin//api"));
    }

    #[test]
    fn versions_in_release_files_become_stars() {
        assert_eq!(release_file_pattern("v1.2.0", "api-v1.2.0-linux.tar.gz"), "api-*-linux.tar.gz");
        assert_eq!(release_file_pattern("v1.2.0", "api-1.2.0-linux.tar.gz"), "api-*-linux.tar.gz");
        assert_eq!(
            release_file_pattern("v0.1.0", "oxploy-website-x86_64-linux.tar.gz"),
            "oxploy-website-x86_64-linux.tar.gz"
        );
        assert_eq!(release_file_pattern("v1", "api-v1-x86_64.tar.gz"), "api-v1-x86_64.tar.gz");
    }

    #[test]
    fn binaries_are_guessed_from_archive_names() {
        assert_eq!(guess_binary("oxploy-website-x86_64-linux.tar.gz"), "oxploy-website");
        assert_eq!(guess_binary("api-v1.2.0-x86_64-unknown-linux-gnu.tar.gz"), "api");
        assert_eq!(guess_binary("api-*-linux.tar.gz"), "api");
        assert_eq!(guess_binary("x86_64-linux.tgz"), "x86_64-linux");
    }
}
