//! Long-term identity keys, and the record of whose key is whose.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use zeroize::ZeroizeOnDrop;

/// Length of a public identity key.
pub const IDENTITY_KEY_LEN: usize = 32;

/// A public identity key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IdentityKey(pub [u8; IDENTITY_KEY_LEN]);

impl IdentityKey {
    /// The raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; IDENTITY_KEY_LEN] {
        &self.0
    }

    /// Lowercase hex, for showing a user which key they are looking at.
    #[must_use]
    pub fn to_hex(&self) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(IDENTITY_KEY_LEN * 2);
        for byte in self.0 {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Whether this key is well-formed.
    ///
    /// Keys arrive over the network; one that is not a valid point can verify
    /// nothing, and knowing that early is better than discovering it per
    /// signature.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.verifying().is_some()
    }

    fn verifying(&self) -> Option<VerifyingKey> {
        VerifyingKey::from_bytes(&self.0).ok()
    }

    /// Whether `signature` over `message` was made by this key.
    #[must_use]
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        let Some(key) = self.verifying() else {
            return false;
        };
        key.verify(message, &Signature::from_bytes(signature))
            .is_ok()
    }
}

/// This installation's long-term key pair.
///
/// The private half never leaves the device. It is what lets a peer recognise
/// the same person across calls, and what a short authentication string is
/// ultimately confirming.
#[derive(ZeroizeOnDrop)]
pub struct Identity {
    #[zeroize(skip)]
    signing: SigningKey,
}

impl std::fmt::Debug for Identity {
    /// Shows only the public half.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("public", &self.public().to_hex())
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Generate a fresh identity.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    /// Restore an identity from its stored private key.
    #[must_use]
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&secret),
        }
    }

    /// The private key, for writing to a keystore.
    #[must_use]
    pub fn to_secret(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// The public key.
    #[must_use]
    pub fn public(&self) -> IdentityKey {
        IdentityKey(self.signing.verifying_key().to_bytes())
    }

    /// Sign a message.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }
}

/// What a key store says about a key we have seen before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Never seen; this is the first call with this account.
    New,
    /// Seen before, and unchanged.
    Known,
    /// Seen before with a *different* key.
    ///
    /// Either they have a new device, or somebody is impersonating them. The
    /// two look identical from here, which is why this must be shown to the
    /// user rather than resolved automatically.
    Changed {
        /// The key previously recorded for this account.
        previously: IdentityKey,
    },
    /// Seen before, unchanged, and confirmed out of band.
    Verified,
}

/// A record of which identity key belongs to which account.
///
/// Trust on first use: the first key seen for an account is remembered, and a
/// later change is reported rather than accepted. A network that can hand out
/// nicknames cannot silently hand out identities too.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnownPeers {
    entries: Vec<KnownPeer>,
}

/// One remembered account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownPeer {
    /// The services account, lowercased.
    pub account: String,
    /// The key recorded for it.
    pub key: IdentityKey,
    /// Whether a short authentication string was confirmed out of band.
    pub verified: bool,
}

impl KnownPeers {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many accounts are remembered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is remembered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every remembered account.
    pub fn iter(&self) -> impl Iterator<Item = &KnownPeer> {
        self.entries.iter()
    }

    fn find(&self, account: &str) -> Option<&KnownPeer> {
        let account = account.to_lowercase();
        self.entries.iter().find(|e| e.account == account)
    }

    /// What we know about this account's key, without recording anything.
    #[must_use]
    pub fn check(&self, account: &str, key: IdentityKey) -> Trust {
        match self.find(account) {
            None => Trust::New,
            Some(entry) if entry.key != key => Trust::Changed {
                previously: entry.key,
            },
            Some(entry) if entry.verified => Trust::Verified,
            Some(_) => Trust::Known,
        }
    }

    /// Record a key for an account, reporting what was true beforehand.
    ///
    /// A changed key is *not* recorded: accepting it silently is exactly the
    /// substitution this store exists to catch. Call [`KnownPeers::replace`]
    /// once the user has decided.
    pub fn observe(&mut self, account: &str, key: IdentityKey) -> Trust {
        let trust = self.check(account, key);
        if trust == Trust::New {
            self.entries.push(KnownPeer {
                account: account.to_lowercase(),
                key,
                verified: false,
            });
        }
        trust
    }

    /// Replace the key recorded for an account, discarding any verification.
    ///
    /// The old confirmation was of the old key; it says nothing about this one.
    pub fn replace(&mut self, account: &str, key: IdentityKey) {
        let account = account.to_lowercase();
        self.entries.retain(|e| e.account != account);
        self.entries.push(KnownPeer {
            account,
            key,
            verified: false,
        });
    }

    /// Record that a short authentication string was confirmed.
    ///
    /// Only marks the account if the key still matches: verifying an account
    /// whose key has since changed would attach the confirmation to the wrong
    /// key.
    pub fn mark_verified(&mut self, account: &str, key: IdentityKey) -> bool {
        let account = account.to_lowercase();
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.account == account && e.key == key)
        else {
            return false;
        };
        entry.verified = true;
        true
    }

    /// Forget an account.
    pub fn forget(&mut self, account: &str) -> bool {
        let account = account.to_lowercase();
        let before = self.entries.len();
        self.entries.retain(|e| e.account != account);
        self.entries.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::{Identity, IdentityKey, KnownPeers, Trust};

    #[test]
    fn a_signature_verifies_against_its_own_key() {
        let identity = Identity::generate();
        let signature = identity.sign(b"a call invitation");
        assert!(identity.public().verify(b"a call invitation", &signature));
    }

    #[test]
    fn a_signature_does_not_verify_against_another_key() {
        let mine = Identity::generate();
        let theirs = Identity::generate();
        let signature = mine.sign(b"a call invitation");
        assert!(!theirs.public().verify(b"a call invitation", &signature));
    }

    #[test]
    fn a_tampered_message_does_not_verify() {
        let identity = Identity::generate();
        let signature = identity.sign(b"call alice");
        assert!(!identity.public().verify(b"call mallory", &signature));
    }

    #[test]
    fn an_identity_survives_being_saved_and_restored() {
        let original = Identity::generate();
        let restored = Identity::from_secret(original.to_secret());
        assert_eq!(original.public(), restored.public());

        let signature = restored.sign(b"message");
        assert!(original.public().verify(b"message", &signature));
    }

    #[test]
    fn the_private_key_does_not_appear_in_debug_output() {
        let identity = Identity::generate();
        let printed = format!("{identity:?}");
        let secret_hex = IdentityKey(identity.to_secret()).to_hex();
        assert!(!printed.contains(&secret_hex), "got {printed}");
        assert!(printed.contains(&identity.public().to_hex()));
    }

    #[test]
    fn a_malformed_key_verifies_nothing_rather_than_panicking() {
        // Keys arrive over the network; one that is not a valid point must
        // fail closed.
        let bogus = IdentityKey([0xff; 32]);
        assert!(!bogus.verify(b"message", &[0; 64]));
    }

    // --- trust on first use -------------------------------------------------

    #[test]
    fn the_first_key_for_an_account_is_remembered() {
        let mut peers = KnownPeers::new();
        let key = Identity::generate().public();

        assert_eq!(peers.observe("alice", key), Trust::New);
        assert_eq!(peers.observe("alice", key), Trust::Known);
        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn account_names_are_matched_without_regard_to_case() {
        let mut peers = KnownPeers::new();
        let key = Identity::generate().public();
        peers.observe("Alice", key);
        assert_eq!(peers.check("ALICE", key), Trust::Known);
    }

    #[test]
    fn a_changed_key_is_reported_and_not_recorded() {
        // Accepting it silently is exactly the substitution this catches.
        let mut peers = KnownPeers::new();
        let original = Identity::generate().public();
        let impostor = Identity::generate().public();
        peers.observe("alice", original);

        assert_eq!(
            peers.observe("alice", impostor),
            Trust::Changed {
                previously: original
            }
        );
        assert_eq!(
            peers.check("alice", original),
            Trust::Known,
            "the original key must still be the one on record"
        );
    }

    #[test]
    fn replacing_a_key_is_deliberate_and_clears_verification() {
        let mut peers = KnownPeers::new();
        let original = Identity::generate().public();
        let replacement = Identity::generate().public();
        peers.observe("alice", original);
        peers.mark_verified("alice", original);

        peers.replace("alice", replacement);
        assert_eq!(
            peers.check("alice", replacement),
            Trust::Known,
            "a replaced key starts unverified: the old confirmation was of the old key"
        );
        assert_eq!(peers.len(), 1, "replacing must not leave both");
    }

    #[test]
    fn verification_sticks_to_the_key_it_was_made_about() {
        let mut peers = KnownPeers::new();
        let key = Identity::generate().public();
        let other = Identity::generate().public();
        peers.observe("alice", key);

        assert!(!peers.mark_verified("alice", other), "wrong key");
        assert!(peers.mark_verified("alice", key));
        assert_eq!(peers.check("alice", key), Trust::Verified);
    }

    #[test]
    fn forgetting_an_account_makes_it_new_again() {
        let mut peers = KnownPeers::new();
        let key = Identity::generate().public();
        peers.observe("alice", key);

        assert!(peers.forget("alice"));
        assert!(!peers.forget("alice"));
        assert_eq!(peers.check("alice", key), Trust::New);
    }
}
