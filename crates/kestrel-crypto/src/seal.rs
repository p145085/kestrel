//! Sealing signalling payloads to a specific peer.
//!
//! The server relays these and cannot read them. That is the whole point: a
//! DTLS fingerprint or an ICE candidate passing through in the clear would let
//! whoever runs the network put themselves in the middle of a call, or learn
//! an address the user believed was hidden behind a cloak.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Length of an ephemeral public key.
pub const EPHEMERAL_KEY_LEN: usize = 32;

/// Length of the nonce prefixed to every sealed payload.
pub const NONCE_LEN: usize = 12;

/// Length of the authentication tag.
pub const TAG_LEN: usize = 16;

/// Bytes a sealed payload adds to its plaintext.
pub const OVERHEAD: usize = NONCE_LEN + TAG_LEN;

/// Why a payload could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    /// The payload was too short to contain a nonce and a tag.
    #[error("sealed payload is truncated")]
    Truncated,
    /// Decryption failed.
    ///
    /// Deliberately carries no detail. A wrong key, a tampered payload and a
    /// replayed one are all the same answer, because distinguishing them tells
    /// an attacker which of those they achieved.
    #[error("sealed payload could not be opened")]
    NotAuthentic,
    /// The counter had already been used, or went backwards.
    #[error("sealed payload was replayed")]
    Replayed,
}

/// One side's ephemeral key pair for a call.
///
/// Ephemeral per call, so the session keys of one call cannot decrypt another
/// even if a long-term key is later compromised.
#[derive(ZeroizeOnDrop)]
pub struct EphemeralKey {
    #[zeroize(skip)]
    secret: StaticSecret,
}

impl std::fmt::Debug for EphemeralKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EphemeralKey").finish_non_exhaustive()
    }
}

impl EphemeralKey {
    /// Generate a fresh key pair.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            secret: StaticSecret::random_from_rng(OsRng),
        }
    }

    /// The public half, to put in an invitation.
    #[must_use]
    pub fn public(&self) -> [u8; EPHEMERAL_KEY_LEN] {
        PublicKey::from(&self.secret).to_bytes()
    }

    /// Agree a shared secret with a peer's public key.
    #[must_use]
    pub fn agree(&self, peer: &[u8; EPHEMERAL_KEY_LEN]) -> SharedSecret {
        let shared = self.secret.diffie_hellman(&PublicKey::from(*peer));
        SharedSecret(shared.to_bytes())
    }
}

/// The raw output of the key agreement.
///
/// Not used directly: keys are derived from it per direction, so the two sides
/// never encrypt with the same key and a message cannot be reflected back.
#[derive(ZeroizeOnDrop)]
pub struct SharedSecret([u8; 32]);

impl std::fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedSecret").finish_non_exhaustive()
    }
}

impl SharedSecret {
    /// The raw bytes, for deriving a short authentication string.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Derive the two directional keys for a call.
    ///
    /// `call_id` binds the keys to this call, so a payload from one call
    /// cannot be replayed into another even between the same two people.
    #[must_use]
    pub fn derive(&self, call_id: &[u8], we_are_initiator: bool) -> SealingKeys {
        let hkdf = Hkdf::<Sha256>::new(Some(call_id), &self.0);
        let mut initiator = [0u8; 32];
        let mut responder = [0u8; 32];
        // `expand` fails only for absurd output lengths; 32 bytes is fine.
        let _ = hkdf.expand(b"kestrel/rtc/v0/initiator", &mut initiator);
        let _ = hkdf.expand(b"kestrel/rtc/v0/responder", &mut responder);

        let (sending, receiving) = if we_are_initiator {
            (initiator, responder)
        } else {
            (responder, initiator)
        };
        initiator.zeroize();
        responder.zeroize();

        SealingKeys {
            sending,
            receiving,
            next_counter: 0,
            highest_seen: None,
        }
    }
}

/// The keys and counters for one direction each of a call.
#[derive(ZeroizeOnDrop)]
pub struct SealingKeys {
    sending: [u8; 32],
    receiving: [u8; 32],
    /// The counter for the next payload we seal.
    #[zeroize(skip)]
    next_counter: u64,
    /// The highest counter accepted so far, if any.
    #[zeroize(skip)]
    highest_seen: Option<u64>,
}

impl std::fmt::Debug for SealingKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealingKeys")
            .field("next_counter", &self.next_counter)
            .field("highest_seen", &self.highest_seen)
            .finish_non_exhaustive()
    }
}

impl SealingKeys {
    /// Seal a payload for the peer.
    ///
    /// Returns the counter used alongside the ciphertext; it travels with the
    /// payload so the peer can reject a replay.
    pub fn seal(&mut self, plaintext: &[u8], associated: &[u8]) -> (u64, Vec<u8>) {
        let counter = self.next_counter;
        self.next_counter = self.next_counter.wrapping_add(1);

        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.sending));
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let aad = aad_for(counter, associated);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            // Encryption fails only on absurd input sizes, which cannot occur
            // for a signalling payload.
            .unwrap_or_default();

        let mut sealed = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        sealed.extend_from_slice(&nonce_bytes);
        sealed.extend_from_slice(&ciphertext);
        (counter, sealed)
    }

    /// Open a payload from the peer.
    pub fn open(
        &mut self,
        counter: u64,
        sealed: &[u8],
        associated: &[u8],
    ) -> Result<Vec<u8>, SealError> {
        if sealed.len() < OVERHEAD {
            return Err(SealError::Truncated);
        }
        // A counter that has already been used, or moved backwards, is a
        // replay. Checked before decrypting, so a replay costs nothing.
        if self.highest_seen.is_some_and(|seen| counter <= seen) {
            return Err(SealError::Replayed);
        }

        let (nonce_bytes, ciphertext) = sealed.split_at(NONCE_LEN);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.receiving));
        let aad = aad_for(counter, associated);

        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(nonce_bytes),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| SealError::NotAuthentic)?;

        // Only advance once the payload is proven authentic; otherwise anybody
        // could burn our counter with garbage and lock out the real peer.
        self.highest_seen = Some(counter);
        Ok(plaintext)
    }
}

/// Bind the counter and caller-supplied context into the authenticated data.
fn aad_for(counter: u64, associated: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(8 + associated.len());
    aad.extend_from_slice(&counter.to_be_bytes());
    aad.extend_from_slice(associated);
    aad
}

#[cfg(test)]
mod tests {
    use super::{EphemeralKey, SealError};

    /// Two sides that have agreed keys for call `c1`.
    fn agreed() -> (super::SealingKeys, super::SealingKeys) {
        let alice = EphemeralKey::generate();
        let bob = EphemeralKey::generate();
        let shared_a = alice.agree(&bob.public());
        let shared_b = bob.agree(&alice.public());
        (shared_a.derive(b"c1", true), shared_b.derive(b"c1", false))
    }

    #[test]
    fn both_sides_agree_the_same_secret() {
        let alice = EphemeralKey::generate();
        let bob = EphemeralKey::generate();
        assert_eq!(
            alice.agree(&bob.public()).as_bytes(),
            bob.agree(&alice.public()).as_bytes()
        );
    }

    #[test]
    fn a_sealed_payload_opens_on_the_other_side() {
        let (mut alice, mut bob) = agreed();
        let (counter, sealed) = alice.seal(b"a session description", b"alice->bob");
        assert_eq!(
            bob.open(counter, &sealed, b"alice->bob").unwrap(),
            b"a session description"
        );
    }

    #[test]
    fn the_ciphertext_does_not_contain_the_plaintext() {
        let (mut alice, _bob) = agreed();
        let (_, sealed) = alice.seal(b"fingerprint", b"");
        assert!(
            !sealed.windows(11).any(|w| w == b"fingerprint"),
            "the payload the server relays must not be readable"
        );
    }

    #[test]
    fn a_tampered_payload_does_not_open() {
        let (mut alice, mut bob) = agreed();
        let (counter, mut sealed) = alice.seal(b"a session description", b"");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert_eq!(
            bob.open(counter, &sealed, b""),
            Err(SealError::NotAuthentic)
        );
    }

    #[test]
    fn changing_the_associated_data_breaks_authentication() {
        // The context a payload was sealed for is part of what it proves.
        let (mut alice, mut bob) = agreed();
        let (counter, sealed) = alice.seal(b"payload", b"alice->bob");
        assert_eq!(
            bob.open(counter, &sealed, b"alice->carol"),
            Err(SealError::NotAuthentic)
        );
    }

    #[test]
    fn a_payload_from_another_call_does_not_open() {
        // The call id is mixed into the key, so the same two people in a
        // different call cannot have each other's payloads replayed at them.
        let alice = EphemeralKey::generate();
        let bob = EphemeralKey::generate();
        let mut first = alice.agree(&bob.public()).derive(b"c1", true);
        let mut other_call = bob.agree(&alice.public()).derive(b"c2", false);

        let (counter, sealed) = first.seal(b"payload", b"");
        assert_eq!(
            other_call.open(counter, &sealed, b""),
            Err(SealError::NotAuthentic)
        );
    }

    #[test]
    fn a_replayed_payload_is_refused() {
        let (mut alice, mut bob) = agreed();
        let (counter, sealed) = alice.seal(b"payload", b"");

        assert!(bob.open(counter, &sealed, b"").is_ok());
        assert_eq!(bob.open(counter, &sealed, b""), Err(SealError::Replayed));
    }

    #[test]
    fn an_older_counter_is_refused_after_a_newer_one() {
        let (mut alice, mut bob) = agreed();
        let (first_counter, first) = alice.seal(b"one", b"");
        let (second_counter, second) = alice.seal(b"two", b"");

        assert!(bob.open(second_counter, &second, b"").is_ok());
        assert_eq!(
            bob.open(first_counter, &first, b""),
            Err(SealError::Replayed)
        );
    }

    #[test]
    fn a_forged_payload_does_not_burn_the_counter() {
        // Otherwise anybody who can reach the relay could lock out the real
        // peer by sending garbage with a high counter.
        let (mut alice, mut bob) = agreed();
        assert_eq!(
            bob.open(1000, &[0u8; 64], b""),
            Err(SealError::NotAuthentic)
        );

        let (counter, sealed) = alice.seal(b"payload", b"");
        assert!(
            bob.open(counter, &sealed, b"").is_ok(),
            "a genuine payload should still be accepted"
        );
    }

    #[test]
    fn a_truncated_payload_is_refused_without_decrypting() {
        let (_alice, mut bob) = agreed();
        assert_eq!(bob.open(0, b"short", b""), Err(SealError::Truncated));
    }

    #[test]
    fn each_direction_uses_a_different_key() {
        // Otherwise a payload could be reflected back at its sender.
        let (mut alice, mut bob) = agreed();
        let (counter, sealed) = alice.seal(b"payload", b"");
        assert_eq!(
            alice.open(counter, &sealed, b""),
            Err(SealError::NotAuthentic),
            "our own payload must not open with our receiving key"
        );
        assert!(bob.open(counter, &sealed, b"").is_ok());
    }

    #[test]
    fn a_third_party_cannot_open_anything() {
        let (mut alice, _bob) = agreed();
        let mallory = EphemeralKey::generate();
        let victim = EphemeralKey::generate();
        let mut intruder = mallory.agree(&victim.public()).derive(b"c1", false);

        let (counter, sealed) = alice.seal(b"payload", b"");
        assert_eq!(
            intruder.open(counter, &sealed, b""),
            Err(SealError::NotAuthentic)
        );
    }
}
