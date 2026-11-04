//! Downloaded files: checking them against a checksum, and unpacking archives. Updates
//! of oxploy and deployments of GitHub releases both download their files.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use super::machine::Machine;

/// The SHA-256 checksum of the file at `path`, in lowercase hexadecimal.
pub fn sha256(path: &Path) -> io::Result<String> {
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

/// The checksum in a `.sha256` file, as `sha256sum` writes it, in lowercase.
pub fn checksum_in(text: &str) -> Option<String> {
    text.split_whitespace()
        .next()
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

/// Whether a file with this name is an archive [`unpack`] opens.
pub fn is_archive(name: &str) -> bool {
    name.ends_with(".tar.gz") || name.ends_with(".tgz")
}

/// Unpacks the `.tar.gz` at `archive` into `into`, with the system's `tar`.
pub fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
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
            "the archive could not be unpacked: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// A file of an archive that may be run.
#[derive(Clone, Debug)]
pub struct Runnable {
    /// Its path in the archive, such as `bin/api`.
    pub path: String,
    /// The machine it is built for, when it is a binary oxploy knows.
    pub machine: Option<Machine>,
}

/// The files of the `.tar.gz` at `archive` that may be run, the ones nearest its top
/// first. Unpacks it into `scratch` to look.
pub fn runnables_in(archive: &Path, scratch: &Path) -> Result<Vec<Runnable>, String> {
    fs::create_dir_all(scratch).map_err(|error| error.to_string())?;
    unpack(archive, scratch)?;
    let mut found = Vec::new();
    walk(scratch, scratch, &mut found).map_err(|error| error.to_string())?;
    found.sort_by_key(|runnable| (runnable.path.matches('/').count(), runnable.path.clone()));
    Ok(found)
}

/// Adds the runnable files under `folder` to `found`, by their path from `root`.
fn walk(root: &Path, folder: &Path, found: &mut Vec<Runnable>) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        // Links are not followed: they could point out of the archive.
        let metadata = entry.path().symlink_metadata()?;
        if metadata.is_dir() {
            walk(root, &entry.path(), found)?;
        } else if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            let path = entry.path();
            if let Ok(relative) = path.strip_prefix(root) {
                found.push(Runnable {
                    path: relative.to_string_lossy().into_owned(),
                    machine: Machine::of_binary(&path).ok().flatten(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_read_as_sha256sum_writes_them() {
        let hash = "7A5BBB418FB54C097FF2B08DAD8C043F45F50E5DBF68214DE2AA762BFC000D62";
        assert_eq!(
            checksum_in(&format!("{hash}  oxploy-x86_64-linux.tar.gz\n")).as_deref(),
            Some(hash.to_ascii_lowercase().as_str())
        );
        assert_eq!(checksum_in("not a checksum"), None);
        assert_eq!(checksum_in(""), None);
    }

    #[test]
    fn archives_show_what_they_can_run() {
        let root = std::env::temp_dir().join(format!("oxploy-runnables-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let packed = root.join("packed");
        fs::create_dir_all(packed.join("bin")).unwrap();
        fs::create_dir_all(packed.join("assets")).unwrap();
        for (path, mode) in [("api", 0o755), ("bin/worker", 0o755), ("assets/app.js", 0o644)] {
            fs::write(packed.join(path), "x").unwrap();
            fs::set_permissions(packed.join(path), fs::Permissions::from_mode(mode)).unwrap();
        }
        let archive = root.join("api.tar.gz");
        let packing = std::process::Command::new("tar")
            .arg("czf")
            .arg(&archive)
            .arg("-C")
            .arg(&packed)
            .args(["api", "bin", "assets"])
            .status()
            .unwrap();
        assert!(packing.success());
        let found = runnables_in(&archive, &root.join("scratch")).unwrap();
        let paths: Vec<&str> = found.iter().map(|runnable| runnable.path.as_str()).collect();
        assert_eq!(paths, ["api", "bin/worker"]);
        assert!(found.iter().all(|runnable| runnable.machine.is_none()));
        fs::remove_dir_all(&root).unwrap();
    }
}
