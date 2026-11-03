//! Where oxploy keeps each app's code, builds, releases and logs on disk.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// The folder for everything oxploy writes besides its database: `OXPLOY_DATA_DIR`, or
/// `data` in the working directory.
static DATA_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = std::env::var_os("OXPLOY_DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from);
    std::path::absolute(&dir).unwrap_or(dir)
});

/// The files of one app, in `apps/<app name>` under the data folder.
#[derive(Clone, Debug)]
pub struct AppFiles {
    root: PathBuf,
}

impl AppFiles {
    /// The files of the app with this app name, which is a slug and so a safe folder
    /// name.
    pub fn of(app_name: &str) -> AppFiles {
        AppFiles {
            root: DATA_DIR.join("apps").join(app_name),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// All releases, one folder per deployment.
    pub fn releases(&self) -> PathBuf {
        self.root.join("releases")
    }

    /// The code and binary one deployment built.
    pub fn release(&self, deployment_id: u64) -> PathBuf {
        self.releases().join(deployment_id.to_string())
    }

    /// The code one deployment fetched, which its binary also runs in.
    pub fn source(&self, deployment_id: u64) -> PathBuf {
        self.release(deployment_id).join("source")
    }

    /// The binary one deployment built.
    pub fn binary(&self, deployment_id: u64, name: &str) -> PathBuf {
        self.release(deployment_id).join("bin").join(name)
    }

    /// The folder the app keeps its data in, such as an SQLite database. It survives
    /// deploys and restarts, and goes only when the service is deleted.
    pub fn storage(&self) -> PathBuf {
        self.root.join("storage")
    }

    /// Copies of the app's databases, one folder per database. They sit outside the
    /// storage folder, so restoring one cannot overwrite them.
    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// The cargo target folder every build of the app shares, so rebuilds only compile
    /// what changed.
    pub fn target(&self) -> PathBuf {
        self.root.join("target")
    }

    /// The remote files Topcoat apps declare with `asset!`, downloaded once for every
    /// build.
    pub fn asset_cache(&self) -> PathBuf {
        self.root.join("asset-cache")
    }

    /// What one deployment printed while fetching, building and starting.
    pub fn deploy_log(&self, deployment_id: u64) -> PathBuf {
        self.root.join("logs").join(format!("deploy-{deployment_id}.log"))
    }

    /// What the running app prints.
    pub fn app_log(&self) -> PathBuf {
        self.root.join("logs").join("app.log")
    }

    /// The running process and the release it runs, so a restarted panel can take it
    /// back.
    pub fn pid_file(&self) -> PathBuf {
        self.root.join("app.pid")
    }
}

/// Returns how many bytes the files in a folder take, with its subfolders. A folder
/// that does not exist takes none.
pub fn size_of(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => size_of(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |metadata| metadata.len()),
            _ => 0,
        })
        .sum()
}

/// Returns the end of a log: its last `lines` lines, read from at most the last 256 KiB.
pub fn tail(path: &Path, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};

    const MAX_BYTES: u64 = 256 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map_or(0, |metadata| metadata.len());
    let start = len.saturating_sub(MAX_BYTES);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    // A read that starts mid-file starts mid-line, so its first line is dropped.
    let text = match start {
        0 => &text[..],
        _ => text.split_once('\n').map_or("", |(_, rest)| rest),
    };
    let kept: Vec<_> = text.lines().rev().take(lines).collect();
    kept.into_iter().rev().collect::<Vec<_>>().join("\n")
}
