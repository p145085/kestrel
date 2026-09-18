//! Accounts and the store that holds them.

use std::collections::HashMap;

use crate::password;

/// A registered account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Account {
    /// The account name in the case it was registered with.
    ///
    /// Stored as bytes because IRC has no guaranteed encoding, and written
    /// as a string on disk so the file stays readable by a human fixing it.
    #[serde(with = "byte_string")]
    pub name: Vec<u8>,
    /// Argon2 PHC string, or `None` for an account that authenticates only by
    /// client certificate.
    pub password_hash: Option<String>,
    /// TLS client certificate fingerprints that may authenticate as this
    /// account, lowercase hex.
    pub certificates: Vec<String>,
    /// When the account was registered, in Unix seconds.
    pub registered_at: u64,
}

/// Why an account could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    /// An account with that name already exists.
    #[error("account already exists")]
    AlreadyExists,
    /// The name is not acceptable.
    #[error("account name is invalid")]
    InvalidName,
    /// The password could not be hashed.
    #[error(transparent)]
    Password(#[from] password::HashError),
}

/// The result of checking credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Authentication succeeded as this account, in its registered case.
    Success(Vec<u8>),
    /// Authentication failed.
    ///
    /// Deliberately carries no detail. Telling a client whether the account
    /// exists turns a password guess into an account enumeration oracle.
    Failure,
}

/// Longest account name accepted.
pub const MAX_ACCOUNT_LEN: usize = 32;

/// Whether `name` is acceptable as an account name.
///
/// Account names are compared ASCII-case-insensitively and must not contain
/// anything that would make them ambiguous with a mask, a mode prefix, or a
/// protocol separator.
#[must_use]
pub fn is_valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ACCOUNT_LEN
        && name[0].is_ascii_alphanumeric()
        && name
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'[' | b']'))
}

/// An in-memory account store.
///
/// Persistence belongs to the server binary, which loads accounts at start-up
/// and writes them back on change; keeping this type free of I/O is what lets
/// authentication be tested without a database.
#[derive(Debug, Clone, Default)]
pub struct AccountStore {
    /// Folded name to account.
    accounts: HashMap<Vec<u8>, Account>,
    /// Certificate fingerprint to folded account name.
    certificates: HashMap<String, Vec<u8>>,
}

impl AccountStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold an account name into its lookup key.
    #[must_use]
    pub fn fold(name: &[u8]) -> Vec<u8> {
        name.to_ascii_lowercase()
    }

    /// Number of registered accounts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    /// Whether the store holds no accounts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Look up an account by name.
    #[must_use]
    pub fn get(&self, name: &[u8]) -> Option<&Account> {
        self.accounts.get(&Self::fold(name))
    }

    /// Every account, for persistence.
    pub fn iter(&self) -> impl Iterator<Item = &Account> {
        self.accounts.values()
    }

    /// Register an account with a password.
    pub fn register(
        &mut self,
        name: &[u8],
        password: &[u8],
        now: u64,
    ) -> Result<(), RegisterError> {
        if !is_valid_name(name) {
            return Err(RegisterError::InvalidName);
        }
        let folded = Self::fold(name);
        if self.accounts.contains_key(&folded) {
            return Err(RegisterError::AlreadyExists);
        }
        let hash = password::hash(password)?;
        self.accounts.insert(
            folded,
            Account {
                name: name.to_vec(),
                password_hash: Some(hash),
                certificates: Vec::new(),
                registered_at: now,
            },
        );
        Ok(())
    }

    /// Insert an account directly, as when loading from storage.
    pub fn insert(&mut self, account: Account) {
        let folded = Self::fold(&account.name);
        for fingerprint in &account.certificates {
            self.certificates
                .insert(fingerprint.to_ascii_lowercase(), folded.clone());
        }
        self.accounts.insert(folded, account);
    }

    /// Associate a client certificate fingerprint with an account.
    pub fn add_certificate(&mut self, name: &[u8], fingerprint: &str) -> bool {
        let folded = Self::fold(name);
        let fingerprint = fingerprint.to_ascii_lowercase();
        let Some(account) = self.accounts.get_mut(&folded) else {
            return false;
        };
        if account.certificates.contains(&fingerprint) {
            return false;
        }
        account.certificates.push(fingerprint.clone());
        self.certificates.insert(fingerprint, folded);
        true
    }

    /// Check a name and password.
    ///
    /// A password is verified even when the account does not exist, against a
    /// dummy hash, so that the time taken does not reveal which accounts are
    /// registered.
    #[must_use]
    pub fn authenticate(&self, name: &[u8], supplied: &[u8]) -> AuthOutcome {
        match self.get(name).and_then(|a| {
            a.password_hash
                .as_deref()
                .map(|hash| (a.name.clone(), hash))
        }) {
            Some((canonical, hash)) if password::verify(supplied, hash) => {
                AuthOutcome::Success(canonical)
            }
            Some(_) => AuthOutcome::Failure,
            None => {
                // Burn comparable time on a known-bad hash.
                let _ = password::verify(supplied, DUMMY_HASH);
                AuthOutcome::Failure
            }
        }
    }

    /// Check a TLS client certificate fingerprint.
    #[must_use]
    pub fn authenticate_certificate(&self, fingerprint: &str) -> AuthOutcome {
        let fingerprint = fingerprint.to_ascii_lowercase();
        match self
            .certificates
            .get(&fingerprint)
            .and_then(|folded| self.accounts.get(folded))
        {
            Some(account) => AuthOutcome::Success(account.name.clone()),
            None => AuthOutcome::Failure,
        }
    }
}

/// Serialise a byte vector as a UTF-8 string where possible.
///
/// Account names are restricted to ASCII, so this is lossless in practice
/// while keeping the saved file legible.
mod byte_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&String::from_utf8_lossy(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        String::deserialize(d).map(String::into_bytes)
    }
}

/// A well-formed Argon2 hash of a value nobody knows, used to equalise the
/// time taken by a failed lookup and a failed password check.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$\
                          8YkKq4xGxWJ0yPGT7dqXvvWqfGIxvGqTWqRJ0Dw9gWk";

#[cfg(test)]
mod tests {
    use super::{AccountStore, AuthOutcome, MAX_ACCOUNT_LEN, RegisterError, is_valid_name};

    fn store_with_alice() -> AccountStore {
        let mut store = AccountStore::new();
        store.register(b"alice", b"hunter2", 0).unwrap();
        store
    }

    #[test]
    fn a_registered_account_authenticates() {
        let store = store_with_alice();
        assert_eq!(
            store.authenticate(b"alice", b"hunter2"),
            AuthOutcome::Success(b"alice".to_vec())
        );
    }

    #[test]
    fn a_wrong_password_fails() {
        let store = store_with_alice();
        assert_eq!(store.authenticate(b"alice", b"wrong"), AuthOutcome::Failure);
    }

    #[test]
    fn an_unknown_account_fails_the_same_way_as_a_wrong_password() {
        // Both must be indistinguishable, or authentication becomes an
        // account enumeration oracle.
        let store = store_with_alice();
        assert_eq!(
            store.authenticate(b"nobody", b"hunter2"),
            AuthOutcome::Failure
        );
        assert_eq!(store.authenticate(b"alice", b"wrong"), AuthOutcome::Failure);
    }

    #[test]
    fn account_names_are_case_insensitive_but_keep_their_case() {
        let store = store_with_alice();
        assert_eq!(
            store.authenticate(b"ALICE", b"hunter2"),
            AuthOutcome::Success(b"alice".to_vec()),
            "lookup should fold case but report the registered spelling"
        );
    }

    #[test]
    fn registering_a_taken_name_is_refused_regardless_of_case() {
        let mut store = store_with_alice();
        assert_eq!(
            store.register(b"Alice", b"other", 0),
            Err(RegisterError::AlreadyExists)
        );
        // The original password still works.
        assert_eq!(
            store.authenticate(b"alice", b"hunter2"),
            AuthOutcome::Success(b"alice".to_vec())
        );
    }

    #[test]
    fn invalid_account_names_are_refused() {
        let mut store = AccountStore::new();
        for bad in [
            &b""[..],
            b"#channel",
            b"has space",
            b"-leading",
            b"_leading",
            b"has@at",
            b"has!bang",
            &[b'a'; MAX_ACCOUNT_LEN + 1],
        ] {
            assert_eq!(
                store.register(bad, b"hunter2", 0),
                Err(RegisterError::InvalidName),
                "should refuse {bad:?}"
            );
        }
    }

    #[test]
    fn valid_names_cover_the_usual_shapes() {
        for good in [
            &b"alice"[..],
            b"Bob99",
            b"a",
            b"with_underscore",
            b"with-dash",
            b"with.dot",
        ] {
            assert!(is_valid_name(good), "should accept {good:?}");
        }
    }

    #[test]
    fn certificate_authentication_works_and_folds_case() {
        let mut store = store_with_alice();
        assert!(store.add_certificate(b"alice", "AABBCC"));

        assert_eq!(
            store.authenticate_certificate("aabbcc"),
            AuthOutcome::Success(b"alice".to_vec())
        );
        assert_eq!(
            store.authenticate_certificate("AABBCC"),
            AuthOutcome::Success(b"alice".to_vec())
        );
        assert_eq!(
            store.authenticate_certificate("ffffff"),
            AuthOutcome::Failure
        );
    }

    #[test]
    fn adding_the_same_certificate_twice_is_a_no_op() {
        let mut store = store_with_alice();
        assert!(store.add_certificate(b"alice", "aabbcc"));
        assert!(!store.add_certificate(b"alice", "AABBCC"));
        assert_eq!(store.get(b"alice").unwrap().certificates.len(), 1);
    }

    #[test]
    fn an_account_reloaded_from_storage_keeps_its_certificates() {
        let mut original = store_with_alice();
        original.add_certificate(b"alice", "aabbcc");
        let saved: Vec<_> = original.iter().cloned().collect();

        let mut reloaded = AccountStore::new();
        for account in saved {
            reloaded.insert(account);
        }

        assert_eq!(
            reloaded.authenticate_certificate("aabbcc"),
            AuthOutcome::Success(b"alice".to_vec())
        );
        assert_eq!(
            reloaded.authenticate(b"alice", b"hunter2"),
            AuthOutcome::Success(b"alice".to_vec())
        );
    }
}
