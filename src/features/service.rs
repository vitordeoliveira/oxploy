//! Services, the Rust apps a project deploys, stored in the database.

use tokio::sync::Mutex;
use topcoat::{Result, context::Cx};

use super::caddy::Caddy;
use super::databases::{self, ManagedDatabase};
use super::deploy::Deployer;
use super::domain::Domain;
use super::environment::{self, EnvironmentError};
use super::github::is_repository_name;
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
}

/// How a service is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, toasty::Embed)]
pub enum BuildKind {
    /// `cargo build --release`, with the options in the cargo settings.
    Cargo,
    /// A shell script the user writes.
    Script,
}

/// A build script, as the user wrote it.
pub struct BuildScript<'a> {
    pub script: &'a str,
    /// The file the script leaves in `OXPLOY_OUTPUT_DIR`.
    pub binary: &'a str,
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

    /// Creates a service in `project`. Returns `None`, creating nothing, if the details
    /// are invalid.
    pub async fn add(cx: &Cx, project: &Project, details: &Details<'_>) -> Result<Option<Service>> {
        let Some((name, description)) = details.normalize() else {
            return Ok(None);
        };
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
            after_build: None,
            environment: None,
            port: None,
            release_id: None,
            stopped_at: None,
        })
        .exec(&mut db(cx))
        .await?;
        Ok(Some(service))
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
fn slug(text: &str) -> String {
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
    use super::*;

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
}
