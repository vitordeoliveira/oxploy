//! Docker, for the services built with a Dockerfile: building their image, and running
//! it in a container on their project's network, published on 127.0.0.1 at the
//! service's port, where a native app listens.
//!
//! oxploy runs the `docker` command, so Docker is optional: without it, only the
//! services built with Docker cannot deploy or run.
//!
//! Each project has a network of its own, which its containers join under their app
//! name, so they reach each other as `http://shop-api:8080`, and no container of
//! another project reaches them.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::environment::{PORT_VARIABLE, STORAGE_VARIABLE};
use super::runtime::system_env;

/// How long oxploy trusts its last look at whether Docker runs.
const LOOK_AGAIN_AFTER: Duration = Duration::from_secs(30);

/// How long `docker version` may take to answer.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// The variables of the panel's environment that tell the docker command where Docker
/// is, which it gets besides [`system_env`].
const DOCKER_ENV: [&str; 5] = ["DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY"];

/// The variable a container gets with the address its app listens at, every one.
pub const HOST_VARIABLE: &str = "HOST";

/// The last look at Docker, and when.
static LAST_LOOK: Mutex<Option<(Instant, Result<String, Unavailable>)>> = Mutex::new(None);

/// Why this server cannot build and run Docker apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// There is no `docker` command on the panel's `PATH`.
    NotInstalled,
    /// The panel's user may not use Docker, as when it is not in the `docker` group.
    NoAccess { user: String },
    /// The `docker` command finds no Docker engine running.
    NotRunning,
    /// Docker did not answer in time.
    NoAnswer,
    /// Docker answered with another error, as it put it.
    Failed(String),
}

impl Unavailable {
    /// What is wrong.
    pub fn problem(&self) -> String {
        match self {
            Self::NotInstalled => "Docker is not installed on this server.".to_owned(),
            Self::NoAccess { user } => format!("Docker is installed, but oxploy's user, {user}, may not use it."),
            Self::NotRunning => "Docker is installed, but it is not running.".to_owned(),
            Self::NoAnswer => format!("Docker did not answer within {} seconds.", VERSION_TIMEOUT.as_secs()),
            Self::Failed(error) => format!("Docker answered with an error: {error}"),
        }
    }

    /// How to fix it, and the command that does, when there is one.
    pub fn fix(&self) -> (&'static str, Option<String>) {
        match self {
            Self::NotInstalled => ("Install Docker, then add oxploy's user to the docker group and restart oxploy.", None),
            Self::NoAccess { user } => (
                "Add it to the docker group, then restart oxploy, so it runs with the group:",
                Some(format!("sudo usermod -aG docker {user} && sudo systemctl restart oxploy")),
            ),
            Self::NotRunning => ("Start it:", Some("sudo systemctl start docker".to_owned())),
            Self::NoAnswer | Self::Failed(_) => ("Check that Docker runs, then try again.", None),
        }
    }

    /// Reads why `docker version` failed from what it printed on standard error.
    fn from_error(stderr: &str, user: &str) -> Unavailable {
        let lower = stderr.to_ascii_lowercase();
        if lower.contains("permission denied") {
            Unavailable::NoAccess { user: user.to_owned() }
        } else if lower.contains("cannot connect to the docker daemon")
            || lower.contains("daemon is running")
            || lower.contains("daemon running")
        {
            Unavailable::NotRunning
        } else {
            let first = stderr.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("it failed");
            Unavailable::Failed(first.chars().take(300).collect())
        }
    }
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (fix, command) = self.fix();
        write!(f, "{} {fix}", self.problem())?;
        if let Some(command) = command {
            write!(f, " {command}")?;
        }
        Ok(())
    }
}

/// The version of the Docker engine on this server, or why it cannot build and run
/// Docker apps. Looked up again at most every [`LOOK_AGAIN_AFTER`].
pub async fn status() -> Result<String, Unavailable> {
    if let Some((at, status)) = &*lock_look()
        && at.elapsed() < LOOK_AGAIN_AFTER
    {
        return status.clone();
    }
    // Boxed, so the pages that ask keep a small future: theirs would otherwise hold the
    // whole of a running command's.
    Box::pin(look_up()).await
}

/// The version of the Docker engine on this server, if it can build and run Docker
/// apps. See [`status`].
pub async fn version() -> Option<String> {
    status().await.ok()
}

/// Asks Docker for its version, and keeps the answer for [`status`].
async fn look_up() -> Result<String, Unavailable> {
    let mut docker = tokio_command();
    docker
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(VERSION_TIMEOUT, docker.output()).await;
    let user = std::env::var("USER").unwrap_or_else(|_| "oxploy".to_owned());
    let status = match output {
        Err(_) => Err(Unavailable::NoAnswer),
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Err(Unavailable::NotInstalled),
        Ok(Err(error)) => Err(Unavailable::Failed(error.to_string())),
        Ok(Ok(output)) => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if output.status.success() && !version.is_empty() {
                Ok(version)
            } else {
                Err(Unavailable::from_error(&String::from_utf8_lossy(&output.stderr), &user))
            }
        }
    };
    *lock_look() = Some((Instant::now(), status.clone()));
    status
}

fn lock_look() -> std::sync::MutexGuard<'static, Option<(Instant, Result<String, Unavailable>)>> {
    LAST_LOOK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The docker command, with only the variables it needs from the panel's environment.
pub fn command() -> Command {
    let mut command = Command::new("docker");
    command.env_clear().envs(system_env()).envs(docker_env());
    command
}

/// The docker command, to run on a task.
pub fn tokio_command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new("docker");
    command.env_clear().envs(system_env()).envs(docker_env());
    command
}

fn docker_env() -> impl Iterator<Item = (&'static str, String)> {
    DOCKER_ENV
        .into_iter()
        .filter_map(|name| Some((name, std::env::var(name).ok()?)))
}

/// The image one deployment of the app built, such as `oxploy-shop-api:42`.
pub fn image(app_name: &str, deployment_id: u64) -> String {
    format!("{}:{deployment_id}", repository(app_name))
}

/// What the app's images are named, before their tag.
fn repository(app_name: &str) -> String {
    format!("oxploy-{app_name}")
}

/// The app's container, such as `oxploy-shop-api`.
pub fn container_name(app_name: &str) -> String {
    format!("oxploy-{app_name}")
}

/// The network of the project with this ID, which its containers share.
pub fn network(project_id: u64) -> String {
    format!("oxploy-project-{project_id}")
}

/// Makes the network, unless it exists.
pub fn ensure_network(name: &str) -> Result<(), String> {
    let exists = command()
        .args(["network", "inspect", "--format", "{{.Name}}", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if exists {
        return Ok(());
    }
    let output = command()
        .args(["network", "create", "--driver", "bridge", "--label", "oxploy=project", name])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("docker could not run: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Made meanwhile by another start.
    if output.status.success() || stderr.contains("already exists") {
        Ok(())
    } else {
        Err(format!("the network {name} could not be made: {}", stderr.trim()))
    }
}

/// Deletes the network, if nothing uses it.
pub fn remove_network(name: &str) {
    quietly(&["network", "rm", name]);
}

/// Deletes the container, stopping it first if it runs.
pub fn remove_container(name: &str) {
    quietly(&["rm", "--force", name]);
}

/// Deletes the app's images, but those of the deployments `keep` says to keep.
pub fn remove_images(app_name: &str, keep: impl Fn(u64) -> bool) {
    let repository = repository(app_name);
    let Ok(output) = command()
        .args(["image", "ls", "--format", "{{.Tag}}", &repository])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    let tags = String::from_utf8_lossy(&output.stdout);
    for tag in tags.lines().map(str::trim) {
        let kept = tag.parse().is_ok_and(&keep);
        if !kept && !tag.is_empty() {
            quietly(&["image", "rm", &format!("{repository}:{tag}")]);
        }
    }
}

/// Runs docker with `args`, and leaves it be if it fails.
fn quietly(args: &[&str]) {
    let _ = command()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A service's container, as oxploy starts it.
#[derive(Clone, Debug)]
pub struct Container {
    pub image: String,
    pub name: String,
    pub network: String,
    /// The name the project's other containers reach it by: its app name.
    pub alias: String,
    /// The port on 127.0.0.1 it is published at.
    pub port: u64,
    /// The port its app listens on inside.
    pub inner_port: u64,
    /// The app's storage folder, at the same path inside, so the variables that name
    /// it hold there too.
    pub storage: PathBuf,
}

/// A `docker run` command: its arguments, and the variables the docker command itself
/// gets, which hand the container the values of the variables named in the arguments.
/// Values stay out of the arguments, which every user of the machine can read.
pub struct Run {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl Run {
    /// Runs the app: `variables` are the app's, oxploy's own aside, which this adds.
    pub fn app(container: &Container, variables: &[(String, String)]) -> Run {
        let mut run = Run::base(container, variables);
        run.args.extend([
            "--name".to_owned(),
            container.name.clone(),
            "--network-alias".to_owned(),
            container.alias.clone(),
            "--publish".to_owned(),
            format!("127.0.0.1:{}:{}", container.port, container.inner_port),
            container.image.clone(),
        ]);
        run
    }

    /// Runs `script` with `sh -e` in a container of the image, as the app would run,
    /// but neither named nor published, for the script after the build.
    pub fn script(container: &Container, variables: &[(String, String)], script: &str) -> Run {
        let mut run = Run::base(container, variables);
        run.args.extend([
            "--entrypoint".to_owned(),
            "sh".to_owned(),
            container.image.clone(),
            "-e".to_owned(),
            "-c".to_owned(),
            script.to_owned(),
        ]);
        run
    }

    /// What every run shares: removed when it exits, an init that passes signals on,
    /// the project's network, the storage folder and the variables.
    fn base(container: &Container, variables: &[(String, String)]) -> Run {
        let storage = container.storage.display().to_string();
        let mut args: Vec<String> = [
            "run",
            "--rm",
            "--init",
            "--network",
            &container.network,
            "--volume",
            &format!("{storage}:{storage}"),
        ]
        .map(str::to_owned)
        .to_vec();
        // HOST first, so a line of the user's can change it; PORT and the storage
        // folder last, as oxploy sets them.
        let mut all: Vec<(String, String)> = vec![(HOST_VARIABLE.to_owned(), "0.0.0.0".to_owned())];
        all.extend(variables.iter().cloned());
        all.push((PORT_VARIABLE.to_owned(), container.inner_port.to_string()));
        all.push((STORAGE_VARIABLE.to_owned(), storage));
        let mut env = Vec::new();
        let mut named = Vec::new();
        for (name, value) in all {
            // The docker command reads these itself, so their values go in the
            // arguments instead.
            if needed_by_docker(&name) {
                args.extend(["--env".to_owned(), format!("{name}={value}")]);
                continue;
            }
            if !named.contains(&name) {
                args.extend(["--env".to_owned(), name.clone()]);
                named.push(name.clone());
            }
            env.push((name, value));
        }
        Run { args, env }
    }

    /// The docker command for this run, without its standard streams.
    pub fn command(&self) -> Command {
        let mut command = command();
        command.args(&self.args).envs(self.env.iter().map(|(name, value)| (name, value)));
        command
    }

    /// The docker command for this run, to run on a task.
    pub fn tokio_command(&self) -> tokio::process::Command {
        let mut command = tokio_command();
        command.args(&self.args).envs(self.env.iter().map(|(name, value)| (name, value)));
        command
    }
}

/// Whether the docker command reads the variable `name` itself.
fn needed_by_docker(name: &str) -> bool {
    DOCKER_ENV.contains(&name) || system_env_names().any(|passed| passed == name)
}

fn system_env_names() -> impl Iterator<Item = &'static str> {
    super::runtime::SYSTEM_ENV.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container() -> Container {
        Container {
            image: image("shop-api", 42),
            name: container_name("shop-api"),
            network: network(7),
            alias: "shop-api".to_owned(),
            port: 10002,
            inner_port: 8080,
            storage: PathBuf::from("/srv/oxploy/apps/shop-api/storage"),
        }
    }

    #[test]
    fn says_why_docker_cannot_be_used() {
        let denied = "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock: Get \"http://%2Fvar%2Frun%2Fdocker.sock/v1.51/version\": dial unix /var/run/docker.sock: connect: permission denied";
        let no_access = Unavailable::from_error(denied, "oxploy");
        assert_eq!(no_access, Unavailable::NoAccess { user: "oxploy".to_owned() });
        assert_eq!(no_access.fix().1.as_deref(), Some("sudo usermod -aG docker oxploy && sudo systemctl restart oxploy"));
        let stopped = "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?";
        assert_eq!(Unavailable::from_error(stopped, "oxploy"), Unavailable::NotRunning);
        let desktop = "failed to connect to the docker API at unix:///Users/me/.docker/run/docker.sock; check if the path is correct and if the daemon is running: dial unix /Users/me/.docker/run/docker.sock: connect: no such file or directory";
        assert_eq!(Unavailable::from_error(desktop, "me"), Unavailable::NotRunning);
        let other = "\nError response from daemon: Docker Desktop is unable to start\n";
        assert_eq!(
            Unavailable::from_error(other, "me"),
            Unavailable::Failed("Error response from daemon: Docker Desktop is unable to start".to_owned())
        );
        assert!(Unavailable::NotInstalled.to_string().starts_with("Docker is not installed on this server."));
    }

    #[test]
    fn names_follow_the_app() {
        assert_eq!(image("shop-api", 42), "oxploy-shop-api:42");
        assert_eq!(container_name("shop-api"), "oxploy-shop-api");
        assert_eq!(network(7), "oxploy-project-7");
    }

    #[test]
    fn runs_the_app_on_its_projects_network_published_on_127_0_0_1() {
        let variables = [
            ("API_KEY".to_owned(), "secret".to_owned()),
            ("HOST".to_owned(), "127.0.0.1".to_owned()),
            ("PATH".to_owned(), "/app/bin".to_owned()),
        ];
        let run = Run::app(&container(), &variables);
        let args = run.args.join(" ");
        assert!(args.starts_with("run --rm --init --network oxploy-project-7 "), "{args}");
        assert!(args.contains("--volume /srv/oxploy/apps/shop-api/storage:/srv/oxploy/apps/shop-api/storage"));
        assert!(args.ends_with("--name oxploy-shop-api --network-alias shop-api --publish 127.0.0.1:10002:8080 oxploy-shop-api:42"), "{args}");
        // Values stay out of the arguments, but for the ones docker reads itself.
        assert!(!args.contains("secret"));
        assert!(args.contains("--env API_KEY "));
        assert!(args.contains("--env PATH=/app/bin "));
        let value = |name: &str| run.env.iter().rev().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
        assert_eq!(value("API_KEY"), Some("secret"));
        assert_eq!(value("PORT"), Some("8080"));
        assert_eq!(value("OXPLOY_STORAGE_DIR"), Some("/srv/oxploy/apps/shop-api/storage"));
        // The user's HOST comes after oxploy's, so it wins.
        assert_eq!(value("HOST"), Some("127.0.0.1"));
        assert_eq!(args.matches("--env HOST ").count(), 1);
    }

    #[test]
    fn runs_the_script_after_the_build_in_the_image() {
        let run = Run::script(&container(), &[], "./migrate");
        let args = run.args.join(" ");
        assert!(args.ends_with("--entrypoint sh oxploy-shop-api:42 -e -c ./migrate"), "{args}");
        assert!(!args.contains("--publish") && !args.contains("--name"));
    }
}
