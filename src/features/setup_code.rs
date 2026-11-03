//! The setup code, which proves that whoever creates the super admin can reach the
//! server itself: a fresh install already answers on the internet, at the server's
//! address, and whoever opened it first would otherwise own the server.
//!
//! oxploy makes one while the server has no super admin, keeps it in the data folder so
//! that restarts keep it too, and prints it in its log.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

use toasty::Db;
use topcoat::context::{Cx, app_context};

use super::app_files::data_dir;
use super::user::User;

/// The characters of a code, without the ones that look alike, such as 0 and O.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// How many characters a code has, in groups of four.
const LEN: usize = 12;

/// The setup code while the server has no super admin.
pub struct SetupCode {
    code: Mutex<Option<String>>,
}

impl SetupCode {
    /// Makes the code if the server has no super admin, or takes back the one an earlier
    /// run made, and prints it in the log with the address to open.
    pub async fn start(db: &Db, open_at: &str) -> toasty::Result<SetupCode> {
        let code = if User::super_admin_exists_in(db).await? {
            let _ = fs::remove_file(Self::path());
            None
        } else {
            let kept = fs::read_to_string(Self::path())
                .ok()
                .map(|code| code.trim().to_owned())
                .filter(|code| normalize(code).len() == LEN);
            let code = kept.unwrap_or_else(|| {
                let code = generate();
                if let Err(error) = save(&code) {
                    eprintln!("oxploy: the setup code could not be kept: {error}");
                }
                code
            });
            eprintln!(
                "oxploy: the server has no super admin yet. Open {open_at} and enter the setup code {code}"
            );
            Some(code)
        };
        Ok(SetupCode {
            code: Mutex::new(code),
        })
    }

    /// Returns the setup code as registered on the router.
    pub fn get(cx: &Cx) -> &SetupCode {
        app_context::<SetupCode>(cx)
    }

    /// Where the code is kept while it is needed.
    pub fn path() -> PathBuf {
        data_dir().join("setup-code")
    }

    /// Whether `typed` is the code, whatever its case, spaces and dashes.
    pub fn accepts(&self, typed: &str) -> bool {
        let code = self.code.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        code.as_deref().is_some_and(|code| {
            let (code, typed) = (normalize(code), normalize(typed));
            // Compares every character, so the time taken tells nothing of a match.
            code.len() == typed.len()
                && code
                    .bytes()
                    .zip(typed.bytes())
                    .fold(0, |differs, (a, b)| differs | (a ^ b))
                    == 0
        })
    }

    /// Forgets the code, once the super admin exists.
    pub fn used(&self) {
        *self.code.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let _ = fs::remove_file(Self::path());
    }
}

/// A new code, such as `K7QM-2XRP-H9TC`.
fn generate() -> String {
    let mut bytes = [0; LEN];
    getrandom::fill(&mut bytes).expect("the system provides random bytes");
    let characters: Vec<char> = bytes
        .iter()
        .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]))
        .collect();
    characters
        .chunks(4)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// Writes the code where only oxploy's user can read it.
fn save(code: &str) -> std::io::Result<()> {
    fs::create_dir_all(data_dir())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(SetupCode::path())?;
    writeln!(file, "{code}")
}

/// The code's characters alone, in uppercase.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_read_the_same_however_typed() {
        let code = generate();
        assert_eq!(code.len(), LEN + 2);
        let setup = SetupCode {
            code: Mutex::new(Some(code.clone())),
        };
        assert!(setup.accepts(&code));
        assert!(setup.accepts(&code.to_lowercase().replace('-', " ")));
        assert!(!setup.accepts(""));
        assert!(!setup.accepts("AAAA-AAAA-AAAA"));
        setup.code.lock().unwrap().take();
        assert!(!setup.accepts(&code));
    }
}
