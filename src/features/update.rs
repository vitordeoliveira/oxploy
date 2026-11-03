//! Updating oxploy to a newer release from GitHub, in place of the running one.
//!
//! The panel looks for a newer release every hour. Installing one downloads its build
//! for this machine, checks it against the release's checksum and that it runs here, and
//! puts it in place of the running binary and the `assets` folder beside it, keeping
//! those in `previous`. Then the panel runs the new binary in its own place, with the
//! same process ID, so a service manager such as systemd sees no restart. The apps keep
//! running, and the new version takes them back as after any restart.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use topcoat::context::{Cx, app_context};

use super::deploy::Deployer;
use super::project::now;

/// The version of the running panel.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The repository whose releases oxploy updates to.
const REPOSITORY: &str = "vitordeoliveira/oxploy";

/// Where GitHub's API answers.
const GITHUB_API: &str = "https://api.github.com";

/// How often the panel looks for a newer release.
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// How long a downloaded binary may take to print its version.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// How often an update waiting for deployments looks at them again.
const DEPLOYMENTS_INTERVAL: Duration = Duration::from_secs(2);

/// How long open pages get to show that the panel restarts, before it does.
const RESTART_NOTICE: Duration = Duration::from_secs(2);

/// The largest download accepted. A build is about 20 MB.
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// The folder beside the binary that its assets are in.
const ASSETS: &str = "assets";

/// The folder beside the binary that an update is prepared in.
const WORK: &str = ".update";

/// The folder beside the binary that keeps the version the last update replaced.
const PREVIOUS: &str = "previous";

/// Asks GitHub about releases. Statuses other than success are handled by the caller.
static API: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent("oxploy")
        .build()
        .into()
});

/// Downloads builds, which takes longer than asking about them.
static DOWNLOADS: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10 * 60)))
        .user_agent("oxploy")
        .build()
        .into()
});

/// A release of oxploy on GitHub.
#[derive(Clone, Debug)]
pub struct Release {
    /// Its version, such as `0.2.0`.
    pub version: String,
    /// Its page on GitHub, with its notes.
    pub url: String,
    /// Its build for this machine, if it has one.
    build: Option<Build>,
}

/// Where a release's build for this machine downloads from.
#[derive(Clone, Debug)]
struct Build {
    archive: String,
    /// The file with the archive's SHA-256 checksum.
    checksum: String,
}

/// What the panel last learned of oxploy's releases.
#[derive(Clone, Debug, Default)]
pub struct Check {
    /// When it last asked GitHub, in seconds since the Unix epoch.
    pub at: Option<i64>,
    /// The latest release, as GitHub last told it.
    pub latest: Option<Release>,
    /// Why the last question failed, if it did.
    pub error: Option<String>,
}

/// How far the installation of an update got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Downloading,
    /// Checking the download against its checksum, and that it runs here.
    Checking,
    /// Waiting for this many deployments, which the restart would cut short.
    WaitingForDeployments(usize),
    Restarting,
    Failed(String),
}

/// An update being installed, or the last one, which failed.
#[derive(Clone, Debug)]
pub struct Installation {
    pub version: String,
    pub step: Step,
}

impl Installation {
    /// Whether it is still being installed.
    pub fn is_running(&self) -> bool {
        !matches!(self.step, Step::Failed(_))
    }
}

/// Why the panel cannot install a release on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    /// It is a development build, which updates from its code.
    DevelopmentBuild,
    /// The release has no build for this machine, named as in the release's files,
    /// such as `aarch64-macos`.
    NoBuild(String),
    /// The panel's user cannot change the folder it is installed in.
    ReadOnly(PathBuf),
    /// The panel could not find its own binary.
    NoBinary,
}

/// The releases of oxploy as the panel knows them, and the update it installs.
#[derive(Clone)]
pub struct Updater {
    shared: Arc<Shared>,
}

struct Shared {
    /// The running binary, as found when the panel started, before an update could
    /// move it.
    binary: Option<PathBuf>,
    check: Mutex<Check>,
    installation: Mutex<Option<Installation>>,
}

impl Shared {
    fn step(&self, step: Step) {
        if let Some(installation) = lock(&self.installation).as_mut() {
            installation.step = step;
        }
    }
}

impl Updater {
    /// Starts looking for a newer release every [`CHECK_INTERVAL`], on a task of its own.
    pub fn start() -> Updater {
        let updater = Updater {
            shared: Arc::new(Shared {
                binary: std::env::current_exe().ok(),
                check: Mutex::default(),
                installation: Mutex::default(),
            }),
        };
        let checker = updater.clone();
        tokio::spawn(async move {
            loop {
                checker.check_now().await;
                tokio::time::sleep(CHECK_INTERVAL).await;
            }
        });
        updater
    }

    /// Returns the updater registered on the router.
    pub fn get(cx: &Cx) -> &Updater {
        app_context::<Updater>(cx)
    }

    /// What the panel last learned of the releases.
    pub fn check(&self) -> Check {
        lock(&self.shared.check).clone()
    }

    /// Asks GitHub for the latest release now.
    pub async fn check_now(&self) {
        let answer = latest_release().await;
        let mut check = lock(&self.shared.check);
        check.at = Some(now());
        match answer {
            Ok(release) => {
                check.latest = Some(release);
                check.error = None;
            }
            Err(error) => check.error = Some(error),
        }
    }

    /// The latest release, if it is newer than the running panel.
    pub fn available(&self) -> Option<Release> {
        lock(&self.shared.check)
            .latest
            .clone()
            .filter(|release| is_newer(&release.version, VERSION))
    }

    /// The update being installed, or the last one, which failed.
    pub fn installation(&self) -> Option<Installation> {
        lock(&self.shared.installation).clone()
    }

    /// Whether an update is being installed.
    pub fn is_installing(&self) -> bool {
        lock(&self.shared.installation)
            .as_ref()
            .is_some_and(Installation::is_running)
    }

    /// Why the panel cannot install `release` on its own, if it cannot.
    pub fn blocker(&self, release: &Release) -> Option<Blocker> {
        if cfg!(debug_assertions) {
            return Some(Blocker::DevelopmentBuild);
        }
        if release.build.is_none() {
            return Some(Blocker::NoBuild(machine()));
        }
        let Some(folder) = self.shared.binary.as_deref().and_then(Path::parent) else {
            return Some(Blocker::NoBinary);
        };
        let writable = is_writable(folder) && is_writable(&folder.join(ASSETS));
        (!writable).then(|| Blocker::ReadOnly(folder.to_owned()))
    }

    /// Starts installing `release` on a task of its own, unless the panel cannot install
    /// it or already installs an update. The deployments in progress finish before the
    /// panel restarts on it.
    pub fn install(&self, release: &Release, deployer: Deployer) -> bool {
        if self.blocker(release).is_some() {
            return false;
        }
        let (Some(binary), Some(build)) = (self.shared.binary.clone(), release.build.clone())
        else {
            return false;
        };
        {
            let mut installation = lock(&self.shared.installation);
            if installation.as_ref().is_some_and(Installation::is_running) {
                return false;
            }
            *installation = Some(Installation {
                version: release.version.clone(),
                step: Step::Downloading,
            });
        }
        tokio::spawn(install(
            self.shared.clone(),
            binary,
            release.version.clone(),
            build,
            deployer,
        ));
        true
    }
}

/// Installs the build and runs it in place of the panel, or records why it could not.
async fn install(
    shared: Arc<Shared>,
    binary: PathBuf,
    version: String,
    build: Build,
    deployer: Deployer,
) {
    let Some(folder) = binary.parent().map(Path::to_path_buf) else {
        return shared.step(Step::Failed("the panel could not find its own folder".to_owned()));
    };
    let work = folder.join(WORK);
    let prepare = {
        let (shared, work, version) = (shared.clone(), work.clone(), version.clone());
        move || prepare(&shared, &work, &version, &build)
    };
    let prepared = tokio::task::spawn_blocking(prepare)
        .await
        .unwrap_or_else(|error| Err(error.to_string()));
    if let Err(error) = prepared {
        let _ = fs::remove_dir_all(&work);
        return shared.step(Step::Failed(error));
    }

    // The restart would cut the deployments in progress short, so they finish first.
    loop {
        let deploying = deployer.deploying();
        if deploying > 0 {
            shared.step(Step::WaitingForDeployments(deploying));
            tokio::time::sleep(DEPLOYMENTS_INTERVAL).await;
            continue;
        }
        shared.step(Step::Restarting);
        tokio::time::sleep(RESTART_NOTICE).await;
        if deployer.deploying() == 0 {
            break;
        }
    }

    let swapped = swap(&binary, &work.join("new"), &folder.join(PREVIOUS));
    let _ = fs::remove_dir_all(&work);
    if let Err(error) = swapped {
        return shared.step(Step::Failed(format!(
            "the new version could not be put in place: {error}"
        )));
    }
    eprintln!("oxploy: restarting on version {version}");
    // Runs the new binary in place of this process, with the same arguments and
    // environment. It returns only if that failed.
    let error = Command::new(&binary).args(std::env::args_os().skip(1)).exec();
    shared.step(Step::Failed(format!(
        "version {version} is in place but did not start: {error}. Restart oxploy to run it."
    )));
}

/// Downloads the build into `work`, checks it, and unpacks it into `work/new`.
fn prepare(shared: &Shared, work: &Path, version: &str, build: &Build) -> Result<(), String> {
    let _ = fs::remove_dir_all(work);
    let new = work.join("new");
    fs::create_dir_all(&new)
        .map_err(|error| format!("{} could not be made: {error}", work.display()))?;
    let archive = work.join("build.tar.gz");
    download(&build.archive, &archive)?;

    shared.step(Step::Checking);
    let expected = checksum_at(&build.checksum)?;
    let actual = sha256(&archive).map_err(|error| format!("the download could not be read: {error}"))?;
    if actual != expected {
        return Err("the download does not match the release's checksum".to_owned());
    }
    unpack(&archive, &new)?;
    if !new.join(ASSETS).join("manifest.toml").is_file() {
        return Err("the build has no assets folder".to_owned());
    }
    let printed = version_of(&new.join("oxploy"))?;
    let wanted = format!("oxploy {version}");
    if printed != wanted {
        return Err(format!("the build says it is \"{printed}\", not {wanted}"));
    }
    Ok(())
}

/// Puts the binary and assets in `new` in place of `binary` and the assets beside it,
/// and moves those into `previous`. If a move fails, it moves back the ones before it.
fn swap(binary: &Path, new: &Path, previous: &Path) -> io::Result<()> {
    let folder = binary.parent().unwrap_or(Path::new("."));
    let assets = folder.join(ASSETS);
    let name = binary.file_name().unwrap_or("oxploy".as_ref());
    let _ = fs::remove_dir_all(previous);
    fs::create_dir_all(previous)?;
    let moves = [
        (assets.clone(), previous.join(ASSETS)),
        (new.join(ASSETS), assets),
        (binary.to_owned(), previous.join(name)),
        (new.join("oxploy"), binary.to_owned()),
    ];
    for (done, (from, to)) in moves.iter().enumerate() {
        if let Err(error) = fs::rename(from, to) {
            for (from, to) in moves[..done].iter().rev() {
                let _ = fs::rename(to, from);
            }
            return Err(error);
        }
    }
    Ok(())
}

/// Asks GitHub for oxploy's latest release.
async fn latest_release() -> Result<Release, String> {
    #[derive(Deserialize)]
    struct Answer {
        tag_name: String,
        html_url: String,
        assets: Vec<Asset>,
    }

    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }

    // `ureq` blocks, so the request runs off the async threads.
    let call = || {
        let mut response = API
            .get(format!("{GITHUB_API}/repos/{REPOSITORY}/releases/latest"))
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|error| format!("GitHub could not be reached: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("GitHub answered {}", response.status()));
        }
        response
            .body_mut()
            .read_json::<Answer>()
            .map_err(|error| format!("GitHub's answer could not be read: {error}"))
    };
    let answer = tokio::task::spawn_blocking(call)
        .await
        .map_err(|error| error.to_string())??;
    let archive = build_name();
    let file = |name: &str| {
        answer
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
    };
    let build = file(&archive)
        .zip(file(&format!("{archive}.sha256")))
        .map(|(archive, checksum)| Build { archive, checksum });
    Ok(Release {
        version: answer.tag_name.trim_start_matches('v').to_owned(),
        url: answer.html_url,
        build,
    })
}

/// Downloads `url` into the file at `to`.
fn download(url: &str, to: &Path) -> Result<(), String> {
    let mut response = DOWNLOADS
        .get(url)
        .call()
        .map_err(|error| format!("the download failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("the download failed: GitHub answered {}", response.status()));
    }
    let mut file = File::create(to)
        .map_err(|error| format!("{} could not be written: {error}", to.display()))?;
    let mut body = response.body_mut().with_config().limit(MAX_DOWNLOAD).reader();
    io::copy(&mut body, &mut file).map_err(|error| format!("the download failed: {error}"))?;
    Ok(())
}

/// Reads the checksum in the file at `url`, written by `sha256sum`.
fn checksum_at(url: &str) -> Result<String, String> {
    let unreadable = || "the release's checksum could not be read".to_owned();
    let mut response = DOWNLOADS.get(url).call().map_err(|_| unreadable())?;
    if !response.status().is_success() {
        return Err(unreadable());
    }
    let text = response.body_mut().read_to_string().map_err(|_| unreadable())?;
    text.split_whitespace()
        .next()
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(unreadable)
}

/// The SHA-256 checksum of the file at `path`, in lowercase hexadecimal.
fn sha256(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Unpacks the archive at `archive` into `into`, with the system's `tar`.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("tar could not run: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "the build could not be unpacked: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Runs `binary --version`, which also proves that it runs on this machine, and returns
/// what it printed.
fn version_of(binary: &Path) -> Result<String, String> {
    let mut child = Command::new(binary)
        .arg("--version")
        // Away from the panel's database and folders, should it not know the flag.
        .env_clear()
        .current_dir(binary.parent().unwrap_or(Path::new("/")))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("the new binary does not run here: {error}"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < VERSION_TIMEOUT => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the new binary did not print its version".to_owned());
            }
        }
    };
    if !status.success() {
        return Err(format!("the new binary does not run here: it {status}"));
    }
    let mut printed = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut printed);
    }
    Ok(printed.trim().to_owned())
}

/// Whether this process may add and remove files in `folder`.
fn is_writable(folder: &Path) -> bool {
    let Ok(path) = CString::new(folder.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid C string that `access` only reads.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// This machine as the release's files name it, such as `x86_64-linux`.
fn machine() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// The name of the release's file built for this machine.
fn build_name() -> String {
    format!("oxploy-{}.tar.gz", machine())
}

/// Whether `version` comes after `than`, both such as `0.2.0`.
pub fn is_newer(version: &str, than: &str) -> bool {
    matches!((numbers(version), numbers(than)), (Some(version), Some(than)) if version > than)
}

/// The major, minor and patch numbers of a version such as `0.2.0` or `v0.2.0-rc.1`.
fn numbers(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.trim_start_matches('v').splitn(3, '.');
    let mut next = || {
        parts
            .next()?
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    };
    Some([next()?, next()?, next()?])
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.2.0", "0.1.1"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        assert!(is_newer("0.2.0-rc.1", "0.1.9"));
        assert!(!is_newer("latest", "0.1.1"));
        assert!(!is_newer("0.2", "0.1.1"));
    }

    #[test]
    fn a_failed_swap_puts_everything_back() {
        let root = std::env::temp_dir().join(format!("oxploy-swap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (installed, new) = (root.join("installed"), root.join("new"));
        fs::create_dir_all(installed.join(ASSETS)).unwrap();
        fs::create_dir_all(new.join(ASSETS)).unwrap();
        fs::write(installed.join("oxploy"), "old").unwrap();
        fs::write(installed.join(ASSETS).join("app.js"), "old").unwrap();
        fs::write(new.join(ASSETS).join("app.js"), "new").unwrap();
        let binary = installed.join("oxploy");
        let previous = installed.join(PREVIOUS);

        // The new binary is missing, so the last move fails.
        assert!(swap(&binary, &new, &previous).is_err());
        assert_eq!(fs::read_to_string(&binary).unwrap(), "old");
        assert_eq!(fs::read_to_string(installed.join(ASSETS).join("app.js")).unwrap(), "old");
        assert_eq!(fs::read_to_string(new.join(ASSETS).join("app.js")).unwrap(), "new");

        fs::write(new.join("oxploy"), "new").unwrap();
        swap(&binary, &new, &previous).unwrap();
        assert_eq!(fs::read_to_string(&binary).unwrap(), "new");
        assert_eq!(fs::read_to_string(installed.join(ASSETS).join("app.js")).unwrap(), "new");
        assert_eq!(fs::read_to_string(previous.join("oxploy")).unwrap(), "old");
        assert_eq!(fs::read_to_string(previous.join(ASSETS).join("app.js")).unwrap(), "old");
        fs::remove_dir_all(&root).unwrap();
    }
}
