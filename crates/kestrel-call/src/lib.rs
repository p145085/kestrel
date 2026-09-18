//! The call state machine.
//!
//! This is the piece that joins signalling to media, and it deliberately knows
//! about neither directly. Signalling payloads come in as strings and go out
//! as strings; the media engine is driven through actions and reports back
//! through events. Both seams exist so the whole of a call's logic — key
//! agreement, consent, verification, teardown — can be tested without a
//! socket, a camera, or GStreamer installed.

use std::collections::HashMap;

use kestrel_crypto::{
    EphemeralKey, Identity, IdentityKey, KnownPeers, Sas, SealingKeys, Trust, sas,
};
use kestrel_rtc_proto::{
    Candidate, Frame, MediaWanted, SessionDescription, binding_context, csd, decode_plain,
    encode_plain, frame, sdp,
};

/// Who opened the negotiation.
///
/// Fixed when a call starts and never renegotiated: both ends offering at once
/// is the classic glare failure, and the cheapest way to avoid it is for
/// exactly one end to be allowed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Made the call; creates the offer.
    Caller,
    /// Received it; answers.
    Callee,
}

/// Why a call ended or could not proceed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// The peer is not part of this call.
    #[error("no such peer in this call")]
    NoSuchPeer,
    /// A payload could not be read.
    #[error("malformed signalling payload")]
    BadPayload,
    /// A signature did not verify.
    ///
    /// Either the peer is not who they claim, or something rewrote the
    /// payload in transit. Both are reasons to stop.
    #[error("signature does not verify")]
    BadSignature,
    /// A frame arrived before the exchange that would let it be read.
    #[error("frame arrived out of order")]
    OutOfOrder,
    /// A description could not be understood.
    #[error("malformed session description")]
    BadDescription,
}

/// What the caller should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send this payload to `peer` with `CALL SIGNAL`.
    Signal {
        /// The peer's nickname.
        peer: String,
        /// The payload, ready for the wire.
        payload: String,
    },
    /// Build a peer connection and start gathering.
    ///
    /// Never emitted before consent: an unanswered call must not cause a
    /// single candidate to be gathered, or ringing somebody would tell you
    /// where they are.
    OpenPeer {
        /// The peer's nickname.
        peer: String,
        /// What to send them.
        media: MediaWanted,
    },
    /// Ask the media engine for an offer.
    CreateOffer {
        /// The peer's nickname.
        peer: String,
    },
    /// Ask the media engine for an answer.
    CreateAnswer {
        /// The peer's nickname.
        peer: String,
    },
    /// Apply a description from the peer.
    SetRemoteDescription {
        /// The peer's nickname.
        peer: String,
        /// `offer` or `answer`.
        kind: String,
        /// Full SDP, reconstituted from the compact form.
        sdp: String,
    },
    /// Apply a candidate from the peer.
    AddCandidate {
        /// The peer's nickname.
        peer: String,
        /// Which media section.
        mline_index: u32,
        /// The candidate line.
        candidate: String,
    },
    /// Tear down a peer connection.
    ClosePeer {
        /// The peer's nickname.
        peer: String,
    },
}

/// Something a caller should show a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Somebody is calling. Nothing has been gathered or disclosed yet.
    Ringing {
        /// Their nickname.
        peer: String,
        /// The account behind it, if they are authenticated.
        account: Option<String>,
        /// What they propose to send.
        media: MediaWanted,
    },
    /// A peer's identity is established, with a phrase to compare aloud.
    ///
    /// The phrase is the only check that survives a hostile server, so a
    /// client should show it rather than bury it.
    Verify {
        /// Their nickname.
        peer: String,
        /// The phrase both ends should see.
        sas: Sas,
        /// What was already known about this account's key.
        trust: Trust,
    },
    /// A peer left, or never joined.
    PeerGone {
        /// Their nickname.
        peer: String,
        /// Why.
        reason: String,
    },
    /// Something went wrong with one peer. The call continues without them.
    PeerFailed {
        /// Their nickname.
        peer: String,
        /// Why.
        reason: String,
    },
}

/// What the media engine reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaEvent {
    /// A local description is ready.
    LocalDescription {
        /// The peer it is for.
        peer: String,
        /// `offer` or `answer`.
        kind: String,
        /// Full SDP.
        sdp: String,
    },
    /// A local candidate is ready.
    LocalCandidate {
        /// The peer it is for.
        peer: String,
        /// Which media section.
        mline_index: u32,
        /// The candidate line.
        candidate: String,
    },
    /// A peer connection failed.
    Failed {
        /// The peer.
        peer: String,
        /// Why.
        reason: String,
    },
}

/// How much a client is willing to disclose to reach a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Privacy {
    /// Send every candidate. Best quality; peers learn your address.
    #[default]
    Direct,
    /// Send only relayed candidates, so peers never see your address.
    ///
    /// Needs a relay to be configured. Without one there is nothing to send,
    /// which is a refusal rather than a quiet downgrade.
    RelayOnly,
}

/// What the caller collected and gave out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Things to do.
    pub actions: Vec<Action>,
    /// Things to show.
    pub events: Vec<Event>,
}

impl Outcome {
    fn act(&mut self, action: Action) {
        self.actions.push(action);
    }

    fn show(&mut self, event: Event) {
        self.events.push(event);
    }
}

/// One peer in a call.
struct Peer {
    account: Option<String>,
    role: Role,
    ephemeral: EphemeralKey,
    identity: Option<IdentityKey>,
    keys: Option<SealingKeys>,
    /// Kept so a short authentication string can be derived once both
    /// identities are known.
    shared: Option<kestrel_crypto::SharedSecret>,
    accepted: bool,
}

impl Peer {
    fn new(role: Role, account: Option<String>) -> Self {
        Self {
            account,
            role,
            ephemeral: EphemeralKey::generate(),
            identity: None,
            keys: None,
            shared: None,
            accepted: false,
        }
    }
}

/// One call, and everyone in it.
pub struct Call {
    id: Vec<u8>,
    /// Our nickname.
    ///
    /// Signatures and seals bind to nicknames rather than accounts because
    /// both ends must derive the same context, and only a logged-in user has
    /// an account. The account still governs whether a key is remembered.
    me: String,
    account: Option<String>,
    identity: Identity,
    known: KnownPeers,
    privacy: Privacy,
    media: MediaWanted,
    peers: HashMap<String, Peer>,
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Call")
            .field("id", &String::from_utf8_lossy(&self.id))
            .field("me", &self.me)
            .field("peers", &self.peers.len())
            .finish_non_exhaustive()
    }
}

impl Call {
    /// Start tracking a call.
    #[must_use]
    pub fn new(
        id: impl Into<Vec<u8>>,
        me: impl Into<String>,
        identity: Identity,
        known: KnownPeers,
    ) -> Self {
        Self {
            id: id.into(),
            me: me.into(),
            account: None,
            identity,
            known,
            privacy: Privacy::default(),
            media: MediaWanted::audio_video(),
            peers: HashMap::new(),
        }
    }

    /// Record the account we are authenticated as, if any.
    ///
    /// Only affects whether our own key is worth remembering to a peer; the
    /// binding is by nickname either way.
    #[must_use]
    pub fn with_account(mut self, account: Option<String>) -> Self {
        self.account = account;
        self
    }

    /// Set what to disclose when gathering.
    #[must_use]
    pub fn with_privacy(mut self, privacy: Privacy) -> Self {
        self.privacy = privacy;
        self
    }

    /// Set what media to offer.
    #[must_use]
    pub fn with_media(mut self, media: MediaWanted) -> Self {
        self.media = media;
        self
    }

    /// What this call discloses.
    #[must_use]
    pub fn privacy(&self) -> Privacy {
        self.privacy
    }

    /// The remembered identity keys, for saving.
    #[must_use]
    pub fn known_peers(&self) -> &KnownPeers {
        &self.known
    }

    /// Whether a peer has accepted and is being connected.
    #[must_use]
    pub fn is_active(&self, peer: &str) -> bool {
        self.peers.get(peer).is_some_and(|p| p.accepted)
    }

    /// Everyone in the call.
    pub fn peers(&self) -> impl Iterator<Item = &str> {
        self.peers.keys().map(String::as_str)
    }

    /// Invite somebody.
    ///
    /// Sends only public keys and a signature: no fingerprint, no candidates,
    /// and nothing gathered yet.
    pub fn invite(&mut self, peer: &str) -> Result<Outcome, CallError> {
        let mut out = Outcome::default();
        let entry = self
            .peers
            .entry(peer.to_owned())
            .or_insert_with(|| Peer::new(Role::Caller, None));

        let context = binding_context(
            &self.id,
            self.me.as_bytes(),
            peer.as_bytes(),
            &entry.ephemeral.public(),
        );
        let frame = Frame::Invite {
            identity: self.identity.public(),
            ephemeral: entry.ephemeral.public(),
            media: self.media,
            signature: self.identity.sign(&context),
        };
        out.act(Action::Signal {
            peer: peer.to_owned(),
            payload: encode_plain(&frame).map_err(|_| CallError::BadPayload)?,
        });
        Ok(out)
    }

    /// Handle a signalling payload from a peer.
    pub fn on_signal(
        &mut self,
        peer: &str,
        account: Option<&str>,
        payload: &str,
    ) -> Result<Outcome, CallError> {
        // An unsealed payload can only be part of the key exchange. Anything
        // else must arrive sealed, which `decode_plain` enforces.
        if let Ok(frame) = decode_plain(payload) {
            return self.on_key_exchange(peer, account, frame);
        }
        self.on_sealed(peer, payload)
    }

    fn on_key_exchange(
        &mut self,
        peer: &str,
        account: Option<&str>,
        frame: Frame,
    ) -> Result<Outcome, CallError> {
        let mut out = Outcome::default();
        match frame {
            Frame::Invite {
                identity,
                ephemeral,
                media,
                signature,
            } => {
                // The signature binds this call, both accounts and the
                // ephemeral key, so it cannot be lifted from another call.
                let context = binding_context(
                    &self.id,
                    peer.as_bytes(),
                    self.me.as_bytes(),
                    &ephemeral,
                );
                if !identity.verify(&context, &signature) {
                    return Err(CallError::BadSignature);
                }

                let entry = self
                    .peers
                    .entry(peer.to_owned())
                    .or_insert_with(|| Peer::new(Role::Callee, account.map(str::to_owned)));
                entry.identity = Some(identity);
                entry.shared = Some(entry.ephemeral.agree(&ephemeral));

                // Ringing only. Nothing is gathered and nothing is disclosed
                // until the user actually answers.
                out.show(Event::Ringing {
                    peer: peer.to_owned(),
                    account: account.map(str::to_owned),
                    media,
                });
            }
            Frame::Accept {
                identity,
                ephemeral,
                signature,
            } => {
                let context = binding_context(
                    &self.id,
                    peer.as_bytes(),
                    self.me.as_bytes(),
                    &ephemeral,
                );
                if !identity.verify(&context, &signature) {
                    return Err(CallError::BadSignature);
                }

                let Some(entry) = self.peers.get_mut(peer) else {
                    return Err(CallError::NoSuchPeer);
                };
                entry.identity = Some(identity);
                let shared = entry.ephemeral.agree(&ephemeral);
                // The caller is the initiator for key derivation, which is
                // what keeps the two directions using different keys.
                entry.keys = Some(shared.derive(&self.id, entry.role == Role::Caller));
                entry.shared = Some(shared);
                entry.accepted = true;
                if entry.account.is_none() {
                    entry.account = account.map(str::to_owned);
                }

                self.emit_verification(peer, &mut out);
                out.act(Action::OpenPeer {
                    peer: peer.to_owned(),
                    media: self.media,
                });
                out.act(Action::CreateOffer {
                    peer: peer.to_owned(),
                });
            }
            _ => return Err(CallError::BadPayload),
        }
        Ok(out)
    }

    fn on_sealed(&mut self, peer: &str, payload: &str) -> Result<Outcome, CallError> {
        let mut out = Outcome::default();
        let context = self.seal_context(peer, false);
        let Some(entry) = self.peers.get_mut(peer) else {
            return Err(CallError::NoSuchPeer);
        };
        // Sealed frames before the exchange has finished cannot be read, and
        // an implementation that sends them is out of order rather than
        // hostile; either way there is nothing to do with it.
        let Some(keys) = entry.keys.as_mut() else {
            return Err(CallError::OutOfOrder);
        };
        let frame = frame::open(payload, keys, &context).map_err(|_| CallError::BadPayload)?;

        match frame {
            Frame::Description(description) => {
                let kind = if entry.role == Role::Caller {
                    "answer"
                } else {
                    "offer"
                };
                out.act(Action::SetRemoteDescription {
                    peer: peer.to_owned(),
                    kind: kind.to_owned(),
                    sdp: sdp::to_sdp(&description),
                });
                // Answering happens only after the offer is applied, so the
                // media engine has something to answer against.
                if kind == "offer" {
                    out.act(Action::CreateAnswer {
                        peer: peer.to_owned(),
                    });
                }
            }
            Frame::Candidates(candidates) => {
                for candidate in candidates {
                    // Section 0 is audio and 1 is video in everything we
                    // generate; a candidate naming neither is audio.
                    let mline_index = u32::from(candidate.mid == "1" || candidate.mid.contains("video"));
                    out.act(Action::AddCandidate {
                        peer: peer.to_owned(),
                        mline_index,
                        candidate: candidate.to_sdp(),
                    });
                }
            }
            Frame::Bye { reason } => {
                self.peers.remove(peer);
                out.act(Action::ClosePeer {
                    peer: peer.to_owned(),
                });
                out.show(Event::PeerGone {
                    peer: peer.to_owned(),
                    reason,
                });
            }
            Frame::Invite { .. } | Frame::Accept { .. } => return Err(CallError::BadPayload),
        }
        Ok(out)
    }

    /// Answer a call that is ringing.
    ///
    /// This is the point at which gathering may begin, and not before.
    pub fn accept(&mut self, peer: &str) -> Result<Outcome, CallError> {
        let mut out = Outcome::default();
        let context = binding_context(
            &self.id,
            self.me.as_bytes(),
            peer.as_bytes(),
            &self
                .peers
                .get(peer)
                .ok_or(CallError::NoSuchPeer)?
                .ephemeral
                .public(),
        );
        let signature = self.identity.sign(&context);
        let public = self.identity.public();

        let Some(entry) = self.peers.get_mut(peer) else {
            return Err(CallError::NoSuchPeer);
        };
        let Some(shared) = entry.shared.take() else {
            return Err(CallError::OutOfOrder);
        };
        let frame = Frame::Accept {
            identity: public,
            ephemeral: entry.ephemeral.public(),
            signature,
        };
        entry.keys = Some(shared.derive(&self.id, entry.role == Role::Caller));
        entry.shared = Some(shared);
        entry.accepted = true;

        out.act(Action::Signal {
            peer: peer.to_owned(),
            payload: encode_plain(&frame).map_err(|_| CallError::BadPayload)?,
        });
        self.emit_verification(peer, &mut out);
        out.act(Action::OpenPeer {
            peer: peer.to_owned(),
            media: self.media,
        });
        Ok(out)
    }

    /// Refuse a call, or leave one.
    pub fn hang_up(&mut self, peer: &str, reason: &str) -> Result<Outcome, CallError> {
        let mut out = Outcome::default();
        let context = self.seal_context(peer, true);
        let Some(entry) = self.peers.get_mut(peer) else {
            return Err(CallError::NoSuchPeer);
        };

        // A peer that never completed the exchange cannot be told in a way
        // they could read; dropping them locally is the whole of it.
        if let Some(keys) = entry.keys.as_mut() {
            let frame = Frame::Bye {
                reason: reason.to_owned(),
            };
            if let Ok(payload) = frame::seal(&frame, keys, &context) {
                out.act(Action::Signal {
                    peer: peer.to_owned(),
                    payload,
                });
            }
        }
        self.peers.remove(peer);
        out.act(Action::ClosePeer {
            peer: peer.to_owned(),
        });
        Ok(out)
    }

    /// Handle something the media engine reported.
    pub fn on_media(&mut self, event: MediaEvent) -> Result<Outcome, CallError> {
        let mut out = Outcome::default();
        match event {
            MediaEvent::LocalDescription { peer, sdp: text, .. } => {
                let description =
                    sdp::from_sdp(&text).ok_or(CallError::BadDescription)?;
                self.seal_to(&peer, &Frame::Description(description), &mut out)?;
            }
            MediaEvent::LocalCandidate {
                peer,
                mline_index,
                candidate,
            } => {
                let mid = if mline_index == 0 {
                    sdp::AUDIO_MID
                } else {
                    sdp::VIDEO_MID
                };
                let Some(parsed) = Candidate::parse_sdp(&candidate, mid) else {
                    // A candidate we cannot parse is one we cannot promise
                    // anything about, so it is dropped rather than forwarded.
                    return Ok(out);
                };
                // Relay-only means exactly that: a candidate that would reveal
                // an address is never sent, whatever the media engine offers.
                if self.privacy == Privacy::RelayOnly && parsed.kind.discloses_address() {
                    return Ok(out);
                }
                self.seal_to(&peer, &Frame::Candidates(vec![parsed]), &mut out)?;
            }
            MediaEvent::Failed { peer, reason } => {
                out.show(Event::PeerFailed { peer, reason });
            }
        }
        Ok(out)
    }

    fn seal_to(&mut self, peer: &str, frame: &Frame, out: &mut Outcome) -> Result<(), CallError> {
        let context = self.seal_context(peer, true);
        let Some(entry) = self.peers.get_mut(peer) else {
            return Err(CallError::NoSuchPeer);
        };
        let Some(keys) = entry.keys.as_mut() else {
            return Err(CallError::OutOfOrder);
        };
        let payload = frame::seal(frame, keys, &context).map_err(|_| CallError::BadPayload)?;
        out.act(Action::Signal {
            peer: peer.to_owned(),
            payload,
        });
        Ok(())
    }

    /// The associated data binding a sealed payload to its direction.
    ///
    /// Directional on purpose: a payload cannot be reflected back at its
    /// sender and still authenticate.
    fn seal_context(&self, peer: &str, outgoing: bool) -> Vec<u8> {
        let (from, to) = if outgoing {
            (self.me.as_str(), peer)
        } else {
            (peer, self.me.as_str())
        };
        let mut context = Vec::with_capacity(self.id.len() + from.len() + to.len() + 3);
        context.extend_from_slice(&self.id);
        context.push(0);
        context.extend_from_slice(from.to_lowercase().as_bytes());
        context.push(0);
        context.extend_from_slice(to.to_lowercase().as_bytes());
        context
    }

    /// Work out what is known about a peer's key and produce the phrase.
    fn emit_verification(&mut self, peer: &str, out: &mut Outcome) {
        let Some(entry) = self.peers.get(peer) else {
            return;
        };
        let (Some(identity), Some(shared)) = (entry.identity, entry.shared.as_ref()) else {
            return;
        };

        let participants = [self.identity.public(), identity];
        let phrase = sas::derive(shared, &self.id, &participants);

        // An unauthenticated peer has no account to pin a key against, so
        // there is nothing to remember and nothing to compare it with.
        let trust = match entry.account.as_deref() {
            Some(account) => self.known.observe(account, identity),
            None => Trust::New,
        };

        out.show(Event::Verify {
            peer: peer.to_owned(),
            sas: phrase,
            trust,
        });
    }

    /// Record that a phrase was confirmed aloud.
    pub fn mark_verified(&mut self, peer: &str) -> bool {
        let Some(entry) = self.peers.get(peer) else {
            return false;
        };
        let (Some(account), Some(identity)) = (entry.account.as_deref(), entry.identity) else {
            return false;
        };
        self.known.mark_verified(account, identity)
    }

    /// Accept a peer's changed key, discarding the old one.
    ///
    /// Only after a user has decided: a changed key and an impersonator look
    /// identical from here.
    pub fn accept_changed_key(&mut self, peer: &str) -> bool {
        let Some(entry) = self.peers.get(peer) else {
            return false;
        };
        let (Some(account), Some(identity)) = (entry.account.as_deref(), entry.identity) else {
            return false;
        };
        self.known.replace(account, identity);
        true
    }
}

/// Build a description the media engine can use from a compact one.
#[must_use]
pub fn expand(description: &SessionDescription) -> String {
    sdp::to_sdp(description)
}

/// Reduce full SDP to the compact form that travels.
#[must_use]
pub fn compact(text: &str) -> Option<SessionDescription> {
    sdp::from_sdp(text)
}

pub use csd::Profile;
