//! Saving and loading the account store.
//!
//! JSON rather than a database: the account store is small, the server keeps
//! it entirely in memory, and a file has no build-time C dependency to carry
//! onto three platforms. Message history, when it arrives, is a different
//! problem with different answers.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kestreld_services::{Account, AccountStore};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// The on-disk shape, versioned so a later format can be recognised.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Saved {
    version: u32,
    accounts: Vec<Account>,
}

/// The format this build writes.
const FORMAT_VERSION: u32 = 1;

/// Read accounts from `path`.
///
/// A missing file is not an error — it is what a server looks like before
/// anybody has registered.
pub fn load(path: &Path) -> Result<Vec<Account>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let saved: Saved =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

    if saved.version > FORMAT_VERSION {
        anyhow::bail!(
            "{} was written by a newer version of kestreld (format {}, this build understands {FORMAT_VERSION})",
            path.display(),
            saved.version
        );
    }
    Ok(saved.accounts)
}

/// Write accounts to `path`.
///
/// Writes to a temporary file in the same directory and renames it into place,
/// so an interrupted save leaves the previous file intact rather than a
/// half-written one. Losing the last account is recoverable; losing all of
/// them is not.
pub fn save(path: &Path, accounts: Vec<Account>) -> Result<()> {
    let saved = Saved {
        version: FORMAT_VERSION,
        accounts,
    };
    let text = serde_json::to_string_pretty(&saved).context("serialising accounts")?;

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let temporary = temporary_path(path);
    std::fs::write(&temporary, text).with_context(|| format!("writing {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Load accounts into a store, logging rather than failing on a bad file.
///
/// A corrupt account file should not stop the server starting: an operator can
/// fix it while the network stays up, which they cannot do if it will not boot.
pub fn load_into(store: &mut AccountStore, path: &Path) {
    match load(path) {
        Ok(accounts) => {
            let count = accounts.len();
            for account in accounts {
                store.insert(account);
            }
            if count > 0 {
                info!(count, path = %path.display(), "loaded accounts");
            }
        }
        Err(error) => {
            warn!(path = %path.display(), %error, "could not load accounts; starting empty");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FORMAT_VERSION, load, load_into, save};
    use kestreld_services::{AccountStore, AuthOutcome};

    fn store_with_alice() -> AccountStore {
        let mut store = AccountStore::new();
        store.register(b"alice", b"hunter2", 0).unwrap();
        store.add_certificate(b"alice", "aabbcc");
        store
    }

    #[test]
    fn accounts_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");

        let original = store_with_alice();
        save(&path, original.iter().cloned().collect()).unwrap();

        let mut reloaded = AccountStore::new();
        load_into(&mut reloaded, &path);

        assert_eq!(
            reloaded.authenticate(b"alice", b"hunter2"),
            AuthOutcome::Success(b"alice".to_vec()),
            "the password should still verify after reloading"
        );
        assert_eq!(
            reloaded.authenticate_certificate("aabbcc"),
            AuthOutcome::Success(b"alice".to_vec()),
            "certificates should be reindexed on load"
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_store_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-written.json");
        assert!(load(&path).unwrap().is_empty());
    }

    #[test]
    fn a_newer_format_is_refused_rather_than_guessed_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        std::fs::write(
            &path,
            format!(r#"{{"version": {}, "accounts": []}}"#, FORMAT_VERSION + 1),
        )
        .unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn a_corrupt_file_does_not_stop_the_server_starting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let mut store = AccountStore::new();
        load_into(&mut store, &path);
        assert!(store.is_empty());
    }

    #[test]
    fn saving_creates_missing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("nested")
            .join("deeper")
            .join("accounts.json");
        save(&path, store_with_alice().iter().cloned().collect()).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn saving_replaces_the_previous_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");

        save(&path, store_with_alice().iter().cloned().collect()).unwrap();
        let mut second = store_with_alice();
        second.register(b"bob", b"hunter2", 0).unwrap();
        save(&path, second.iter().cloned().collect()).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        // No temporary file is left behind.
        assert!(!dir.path().join("accounts.json.tmp").exists());
    }
}
