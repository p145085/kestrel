//! Signalling frames and the sealed envelope that carries them.
//!
//! What goes over IRC is the envelope: a counter, an ephemeral public key
//! where one is needed, and ciphertext. The server relays it and can read none
//! of it.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64;
use kestrel_crypto::{IdentityKey, SealError, SealingKeys};
use serde::{Deserialize, Serialize};

use crate::csd::{Candidate, SessionDescription, serde_bytes_array};

/// What a call's media is meant to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaWanted {
    /// Audio.
    #[serde(rename = "a")]
    pub audio: bool,
    /// Video.
    #[serde(rename = "v")]
    pub video: bool,
    /// A shared screen.
    #[serde(rename = "s")]
    pub screen: bool,
}

impl MediaWanted {
    /// Audio and video.
    #[must_use]
    pub fn audio_video() -> Self {
        Self {
            audio: true,
            video: true,
            screen: false,
        }
    }
}

/// One signalling message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frame {
    /// Offered before either side has committed to anything.
    ///
    /// Deliberately carries no fingerprint and no candidates. An invitation
    /// that leaked either would let anyone learn a callee's address simply by
    /// ringing them, whether or not they answered.
    Invite {
        /// The caller's long-term identity key.
        identity: IdentityKey,
        /// The caller's ephemeral key for this call.
        #[serde(with = "serde_bytes_array")]
        ephemeral: [u8; 32],
        /// What the caller proposes to send.
        media: MediaWanted,
        /// Signature over the invitation's binding context.
        #[serde(with = "serde_bytes_array")]
        signature: [u8; 64],
    },
    /// The callee has agreed to talk. Only now may either side gather.
    Accept {
        /// The callee's long-term identity key.
        identity: IdentityKey,
        /// The callee's ephemeral key for this call.
        #[serde(with = "serde_bytes_array")]
        ephemeral: [u8; 32],
        /// Signature over the acceptance's binding context.
        #[serde(with = "serde_bytes_array")]
        signature: [u8; 64],
    },
    /// An offer or answer.
    Description(SessionDescription),
    /// A batch of trickled candidates.
    ///
    /// Batched rather than sent one at a time: each is a line through a chat
    /// server, and a handful of lines per peer is the difference between a
    /// call that connects and one that trips a flood limit.
    Candidates(Vec<Candidate>),
    /// Leaving, with a reason for the other side to show.
    Bye {
        /// Why.
        reason: String,
    },
}

/// Why a frame could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The envelope was not valid base64url.
    #[error("payload is not valid base64url")]
    NotBase64,
    /// The envelope did not decode.
    #[error("envelope is malformed")]
    MalformedEnvelope,
    /// The sealed contents did not open.
    #[error(transparent)]
    Seal(#[from] SealError),
    /// The plaintext was not a frame.
    #[error("frame is malformed")]
    MalformedFrame,
    /// A frame that must be sealed was handled in the clear.
    #[error("this frame must be sealed")]
    MustBeSealed,
    /// The frame was larger than the transport permits.
    #[error("frame is larger than {max} bytes")]
    TooLarge {
        /// The limit.
        max: usize,
    },
}

/// Largest encoded envelope accepted.
///
/// The server caps what it will relay; refusing earlier means a hostile peer
/// cannot make us allocate for something that would be rejected anyway.
pub const MAX_ENVELOPE: usize = 8192;

/// The envelope that travels over IRC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Envelope {
    /// Replay counter.
    #[serde(rename = "n")]
    counter: u64,
    /// Sealed frame.
    #[serde(rename = "c")]
    ciphertext: Vec<u8>,
}

/// Seal a frame for a peer.
///
/// `context` binds the payload to this call and this direction, so a frame
/// cannot be lifted into another conversation.
pub fn seal(frame: &Frame, keys: &mut SealingKeys, context: &[u8]) -> Result<String, FrameError> {
    let mut plaintext = Vec::new();
    ciborium::into_writer(frame, &mut plaintext).map_err(|_| FrameError::MalformedFrame)?;

    let (counter, ciphertext) = keys.seal(&plaintext, context);
    let envelope = Envelope {
        counter,
        ciphertext,
    };

    let mut encoded = Vec::new();
    ciborium::into_writer(&envelope, &mut encoded).map_err(|_| FrameError::MalformedEnvelope)?;

    let text = BASE64.encode(&encoded);
    if text.len() > MAX_ENVELOPE {
        return Err(FrameError::TooLarge { max: MAX_ENVELOPE });
    }
    Ok(text)
}

/// Open a frame from a peer.
pub fn open(payload: &str, keys: &mut SealingKeys, context: &[u8]) -> Result<Frame, FrameError> {
    if payload.len() > MAX_ENVELOPE {
        return Err(FrameError::TooLarge { max: MAX_ENVELOPE });
    }
    let encoded = BASE64.decode(payload).map_err(|_| FrameError::NotBase64)?;
    let envelope: Envelope =
        ciborium::from_reader(encoded.as_slice()).map_err(|_| FrameError::MalformedEnvelope)?;

    let plaintext = keys.open(envelope.counter, &envelope.ciphertext, context)?;
    ciborium::from_reader(plaintext.as_slice()).map_err(|_| FrameError::MalformedFrame)
}

/// Encode a frame without sealing it.
///
/// Only for the two frames that carry the key exchange itself: there is no
/// shared secret yet, so there is nothing to seal with. Both are safe in the
/// clear because neither carries a fingerprint, a candidate or anything else
/// private — an invitation discloses only that somebody is calling, which the
/// server routing it knows anyway. Everything after is sealed.
pub fn encode_plain(frame: &Frame) -> Result<String, FrameError> {
    match frame {
        Frame::Invite { .. } | Frame::Accept { .. } => {}
        _ => return Err(FrameError::MustBeSealed),
    }
    let mut encoded = Vec::new();
    ciborium::into_writer(frame, &mut encoded).map_err(|_| FrameError::MalformedFrame)?;
    let text = BASE64.encode(&encoded);
    if text.len() > MAX_ENVELOPE {
        return Err(FrameError::TooLarge { max: MAX_ENVELOPE });
    }
    Ok(text)
}

/// Decode an unsealed frame.
///
/// Refuses anything that should have been sealed, so a peer cannot downgrade a
/// description or a candidate batch into the clear simply by sending it that
/// way.
pub fn decode_plain(payload: &str) -> Result<Frame, FrameError> {
    if payload.len() > MAX_ENVELOPE {
        return Err(FrameError::TooLarge { max: MAX_ENVELOPE });
    }
    let encoded = BASE64.decode(payload).map_err(|_| FrameError::NotBase64)?;
    let frame: Frame =
        ciborium::from_reader(encoded.as_slice()).map_err(|_| FrameError::MalformedFrame)?;
    match frame {
        Frame::Invite { .. } | Frame::Accept { .. } => Ok(frame),
        _ => Err(FrameError::MustBeSealed),
    }
}

/// The bytes an invitation or acceptance is signed over.
///
/// Binding the call, both parties and the ephemeral key together is what stops
/// a signature being lifted from one call and presented in another.
#[must_use]
pub fn binding_context(
    call_id: &[u8],
    from_account: &[u8],
    to_account: &[u8],
    ephemeral: &[u8; 32],
) -> Vec<u8> {
    let mut context =
        Vec::with_capacity(call_id.len() + from_account.len() + to_account.len() + 48);
    context.extend_from_slice(b"kestrel/rtc/v0/bind\0");
    context.extend_from_slice(call_id);
    context.push(0);
    context.extend_from_slice(&from_account.to_ascii_lowercase());
    context.push(0);
    context.extend_from_slice(&to_account.to_ascii_lowercase());
    context.push(0);
    context.extend_from_slice(ephemeral);
    context
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use kestrel_crypto::{EphemeralKey, Identity, SealingKeys};

    use super::{Frame, FrameError, MediaWanted, binding_context, open, seal};

    use crate::csd::{Candidate, CandidateKind, Profile, SessionDescription, Setup};

    fn agreed() -> (SealingKeys, SealingKeys) {
        let alice = EphemeralKey::generate();
        let bob = EphemeralKey::generate();
        (
            alice.agree(&bob.public()).derive(b"c1", true),
            bob.agree(&alice.public()).derive(b"c1", false),
        )
    }

    fn description_frame() -> Frame {
        Frame::Description(SessionDescription::new(
            [0x7f; 32],
            "abcd",
            "a-longer-ice-password-value",
            Setup::ActPass,
            Profile::OpusVp8,
            111,
            Some(222),
        ))
    }

    #[test]
    fn a_frame_round_trips_through_the_envelope() {
        let (mut alice, mut bob) = agreed();
        let frame = description_frame();
        let payload = seal(&frame, &mut alice, b"ctx").unwrap();
        assert_eq!(open(&payload, &mut bob, b"ctx").unwrap(), frame);
    }

    #[test]
    fn a_compact_description_stays_small_enough_for_one_line() {
        // The whole reason the format exists: an offer has to fit comfortably
        // in a single protocol line rather than being chunked across several.
        let (mut alice, _bob) = agreed();
        let payload = seal(&description_frame(), &mut alice, b"ctx").unwrap();
        assert!(
            payload.len() < 512,
            "an offer grew to {} bytes",
            payload.len()
        );
    }

    #[test]
    fn every_frame_kind_round_trips() {
        let identity = Identity::generate();
        let frames = vec![
            Frame::Invite {
                identity: identity.public(),
                ephemeral: [1; 32],
                media: MediaWanted::audio_video(),
                signature: [2; 64],
            },
            Frame::Accept {
                identity: identity.public(),
                ephemeral: [3; 32],
                signature: [4; 64],
            },
            description_frame(),
            Frame::Candidates(vec![Candidate {
                foundation: "1".to_owned(),
                component: 1,
                transport: "udp".to_owned(),
                priority: 100,
                address: "10.0.0.1".to_owned(),
                port: 9000,
                kind: CandidateKind::Relay,
                mid: "0".to_owned(),
            }]),
            Frame::Bye {
                reason: "hung up".to_owned(),
            },
        ];

        for frame in frames {
            let (mut alice, mut bob) = agreed();
            let payload = seal(&frame, &mut alice, b"ctx").unwrap();
            assert_eq!(open(&payload, &mut bob, b"ctx").unwrap(), frame);
        }
    }

    #[test]
    fn the_envelope_is_url_safe_so_it_survives_a_protocol_line() {
        let (mut alice, _bob) = agreed();
        let payload = seal(&description_frame(), &mut alice, b"ctx").unwrap();
        assert!(
            payload
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "got {payload}"
        );
    }

    #[test]
    fn a_frame_sealed_for_another_context_does_not_open() {
        let (mut alice, mut bob) = agreed();
        let payload = seal(&description_frame(), &mut alice, b"alice->bob").unwrap();
        assert!(matches!(
            open(&payload, &mut bob, b"alice->carol"),
            Err(FrameError::Seal(_))
        ));
    }

    #[test]
    fn a_tampered_envelope_does_not_open() {
        let (mut alice, mut bob) = agreed();
        let sealed = seal(&description_frame(), &mut alice, b"ctx").unwrap();

        // Changed in the middle rather than at the end. The last base64
        // character carries padding bits, so several values there decode to
        // the same bytes -- tampering only with it would be undone by the
        // decoder, and the test would pass or fail depending on the nonce.
        let middle = sealed.len() / 2;
        let mut payload = sealed.clone();
        let replacement = if sealed.as_bytes()[middle] == b'A' {
            "B"
        } else {
            "A"
        };
        payload.replace_range(middle..=middle, replacement);
        assert_ne!(payload, sealed, "the envelope was not actually changed");

        assert!(open(&payload, &mut bob, b"ctx").is_err());
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        let (_alice, mut bob) = agreed();
        for bad in ["", "!!!!not base64!!!!", "AAAA", "-_-_-_-_"] {
            assert!(
                open(bad, &mut bob, b"ctx").is_err(),
                "should refuse {bad:?}"
            );
        }
    }

    #[test]
    fn an_oversized_payload_is_refused_before_decoding() {
        let (_alice, mut bob) = agreed();
        let huge = "A".repeat(super::MAX_ENVELOPE + 1);
        assert_eq!(
            open(&huge, &mut bob, b"ctx"),
            Err(FrameError::TooLarge {
                max: super::MAX_ENVELOPE
            })
        );
    }

    #[test]
    fn an_invitation_carries_no_fingerprint() {
        // Ringing somebody must not tell you where they are, whether or not
        // they answer.
        let identity = Identity::generate();
        let invite = Frame::Invite {
            identity: identity.public(),
            ephemeral: [1; 32],
            media: MediaWanted::audio_video(),
            signature: [2; 64],
        };
        let mut encoded = Vec::new();
        ciborium::into_writer(&invite, &mut encoded).unwrap();

        // The fingerprint field name only exists in a description.
        let text = String::from_utf8_lossy(&encoded);
        assert!(!text.contains("Description"), "got {text}");
    }

    // --- binding ----------------------------------------------------------

    #[test]
    fn the_key_exchange_frames_travel_in_the_clear() {
        let identity = Identity::generate();
        let invite = Frame::Invite {
            identity: identity.public(),
            ephemeral: [1; 32],
            media: MediaWanted::audio_video(),
            signature: [2; 64],
        };
        let encoded = super::encode_plain(&invite).unwrap();
        assert_eq!(super::decode_plain(&encoded).unwrap(), invite);
    }

    #[test]
    fn nothing_else_may_travel_in_the_clear() {
        // Otherwise a peer could downgrade a description out of its seal
        // simply by sending it unsealed.
        assert_eq!(
            super::encode_plain(&description_frame()),
            Err(FrameError::MustBeSealed)
        );

        let mut encoded = Vec::new();
        ciborium::into_writer(&description_frame(), &mut encoded).unwrap();
        let smuggled = super::BASE64.encode(&encoded);
        assert_eq!(super::decode_plain(&smuggled), Err(FrameError::MustBeSealed));
    }

    #[test]
    fn a_signature_over_the_binding_context_verifies() {
        let identity = Identity::generate();
        let context = binding_context(b"c1", b"alice", b"bob", &[7; 32]);
        let signature = identity.sign(&context);
        assert!(identity.public().verify(&context, &signature));
    }

    #[test]
    fn a_signature_does_not_transfer_to_another_call_or_peer() {
        let identity = Identity::generate();
        let original = binding_context(b"c1", b"alice", b"bob", &[7; 32]);
        let signature = identity.sign(&original);

        for other in [
            binding_context(b"c2", b"alice", b"bob", &[7; 32]),
            binding_context(b"c1", b"alice", b"carol", &[7; 32]),
            binding_context(b"c1", b"mallory", b"bob", &[7; 32]),
            binding_context(b"c1", b"alice", b"bob", &[8; 32]),
        ] {
            assert!(
                !identity.public().verify(&other, &signature),
                "a signature must not carry over"
            );
        }
    }

    #[test]
    fn account_names_bind_without_regard_to_case() {
        assert_eq!(
            binding_context(b"c1", b"Alice", b"BOB", &[7; 32]),
            binding_context(b"c1", b"alice", b"bob", &[7; 32])
        );
    }

    #[test]
    fn the_separators_stop_fields_running_together() {
        // Without them "ab" + "c" and "a" + "bc" would bind identically.
        assert_ne!(
            binding_context(b"c1", b"ab", b"c", &[7; 32]),
            binding_context(b"c1", b"a", b"bc", &[7; 32])
        );
    }
}
