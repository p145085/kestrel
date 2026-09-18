//! Remembering who we are, and who we have spoken to.
//!
//! An identity generated afresh each run makes verification theatre: every
//! call reports the other side's key as new, so a key that changed because
//! somebody is impersonating them looks exactly like a key that changed
//! because they restarted. Keeping both halves -- our own key and the ones we
//! have pinned -- is what makes the spoken phrase worth saying.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kestrel_crypto::{Identity, KnownPeers};
use serde::{Deserialize, Serialize};

/// The format version, so a future change can be recognised rather than
/// misread as corruption.
const VERSION: u8 = 1;

/// What is written to disk.
#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    version: u8,
    /// Our long-term signing key, as hex.
    ///
    /// In the clear, like an unencrypted SSH key: the file is readable only by
    /// its owner, and a passphrase that has to be typed before every call is a
    /// passphrase nobody will keep using. Worth revisiting if Kestrel ever
    /// holds anything an attacker would want more than a call.
    secret: String,
    /// The keys we have pinned against accounts.
    #[serde(default)]
    peers: KnownPeers,
}

/// Where an identity is kept between runs.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// A store at a particular file.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The usual place for this user, if there is one.
    ///
    /// Returns `None` when the platform has told us nothing about where a
    /// user's files live, which is a reason to carry on without remembering
    /// rather than a reason to refuse to start.
    #[must_use]
    pub fn in_config_directory() -> Option<Self> {
        // An explicit path wins, which is how two clients on one machine get
        // separate identities -- and how this gets tested at all, since both
        // ends of a test call would otherwise share one key and one table.
        if let Some(path) = std::env::var_os("KESTREL_IDENTITY") {
            return Some(Self::at(path));
        }

        let base = if cfg!(windows) {
            std::env::var_os("APPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
                })
        }?;
        Some(Self::at(base.join("kestrel").join("identity.json")))
    }

    /// Where this store keeps its file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load what was saved, creating an identity the first time.
    ///
    /// A file that cannot be read is an error rather than a fresh start: a
    /// corrupt or unreadable store would otherwise silently discard every key
    /// the user had verified, which is precisely the event the warnings about
    /// changed keys exist to make visible.
    pub fn load(&self) -> Result<(Identity, KnownPeers)> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let identity = Identity::generate();
                let peers = KnownPeers::new();
                self.save(&identity, &peers)
                    .with_context(|| format!("creating {}", self.path.display()))?;
                return Ok((identity, peers));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", self.path.display()));
            }
        };

        let saved: Saved = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a Kestrel identity", self.path.display()))?;
        if saved.version != VERSION {
            anyhow::bail!(
                "{} was written by a different version of Kestrel",
                self.path.display()
            );
        }

        let secret = decode_secret(&saved.secret)
            .with_context(|| format!("{} holds a malformed key", self.path.display()))?;
        Ok((Identity::from_secret(secret), saved.peers))
    }

    /// Write both halves out.
    pub fn save(&self, identity: &Identity, peers: &KnownPeers) -> Result<()> {
        if let Some(directory) = self.path.parent() {
            std::fs::create_dir_all(directory)
                .with_context(|| format!("creating {}", directory.display()))?;
        }

        let saved = Saved {
            version: VERSION,
            secret: encode_secret(&identity.to_secret()),
            peers: peers.clone(),
        };
        let text = serde_json::to_string_pretty(&saved).context("encoding the identity")?;

        // Written beside and renamed over, so an interrupted save leaves the
        // previous identity intact rather than a half-written file that would
        // read as somebody else.
        let temporary = self.path.with_extension("tmp");
        std::fs::write(&temporary, text.as_bytes())
            .with_context(|| format!("writing {}", temporary.display()))?;
        restrict(&temporary);
        std::fs::rename(&temporary, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))?;
        Ok(())
    }
}

/// Keep the file to its owner where the platform has a say in it.
///
/// On Windows a file under the user's own profile is already theirs alone, so
/// there is nothing to tighten.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn encode_secret(secret: &[u8; 32]) -> String {
    use std::fmt::Write;

    secret
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

fn decode_secret(text: &str) -> Result<[u8; 32]> {
    anyhow::ensure!(text.len() == 64, "a key is 64 hex characters");
    let mut secret = [0u8; 32];
    for (index, byte) in secret.iter_mut().enumerate() {
        let at = index * 2;
        *byte = u8::from_str_radix(&text[at..at + 2], 16).context("not hexadecimal")?;
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = Store::at(directory.path().join("identity.json"));
        (directory, store)
    }

    #[test]
    fn an_identity_survives_a_restart() {
        let (_keep, store) = temporary();

        let (first, _) = store.load().expect("the first load should create one");
        let (second, _) = store.load().expect("the second load should read it back");

        assert_eq!(
            first.public(),
            second.public(),
            "a key that changes every run makes verification meaningless"
        );
    }

    #[test]
    fn a_verified_peer_is_remembered() {
        let (_keep, store) = temporary();
        let (identity, mut peers) = store.load().expect("should load");

        let theirs = Identity::generate().public();
        peers.observe("alice", theirs);
        assert!(peers.mark_verified("alice", theirs), "should verify");
        store.save(&identity, &peers).expect("should save");

        let (_, read_back) = store.load().expect("should load again");
        assert!(
            matches!(
                read_back.check("alice", theirs),
                kestrel_crypto::Trust::Verified
            ),
            "verifying somebody has to outlast the call it happened in"
        );
    }

    #[test]
    fn a_different_key_for_a_known_account_is_still_reported_as_changed() {
        let (_keep, store) = temporary();
        let (identity, mut peers) = store.load().expect("should load");

        let theirs = Identity::generate().public();
        peers.observe("alice", theirs);
        store.save(&identity, &peers).expect("should save");

        let (_, read_back) = store.load().expect("should load again");
        let impostor = Identity::generate().public();
        assert!(
            matches!(
                read_back.check("alice", impostor),
                kestrel_crypto::Trust::Changed { .. }
            ),
            "this is the whole point of keeping the table"
        );
    }

    #[test]
    fn a_damaged_store_is_refused_rather_than_silently_replaced() {
        let (_keep, store) = temporary();
        store.load().expect("should create one");
        std::fs::write(store.path(), "{ this is not json").expect("should write");

        // Starting fresh here would throw away every pinned key without
        // saying so, which is exactly what an attacker would want.
        assert!(store.load().is_err());
    }
}
