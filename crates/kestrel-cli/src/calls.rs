//! Placing and answering calls from the terminal client.
//!
//! Joins the three pieces that were built separately: `CALL` messages over
//! IRC, the call state machine that decides what they mean, and the media
//! engine that actually carries audio and video.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use kestrel_call::{Action, Call, Event, MediaEvent, Privacy};
use kestrel_crypto::{Identity, KnownPeers, Trust};
use kestrel_media::{ConnectionState, PeerConnection, PeerEvent, Sending, Source};
use kestrel_net::Handle;
use kestrel_proto::MessageBuf;
use kestrel_rtc_proto::MediaWanted;
use tokio::sync::mpsc;

use crate::ui::{colour, status, warn};

/// A media event tagged with the peer it came from.
pub struct TaggedMediaEvent {
    /// The peer.
    pub peer: String,
    /// What happened.
    pub event: PeerEvent,
}

/// Everything the client knows about calls in progress.
pub struct Calls {
    identity: Identity,
    known: KnownPeers,
    privacy: Privacy,
    /// Our nickname, which is what signalling binds to.
    nick: String,
    /// Our account, once authenticated. Only a logged-in peer has a key worth
    /// remembering, so an unauthenticated user is warned rather than silently
    /// given weaker guarantees.
    account: Option<String>,
    /// Active calls, by the server's call id.
    active: HashMap<String, ActiveCall>,
    /// Peer connections, keyed by call id and peer nickname.
    connections: HashMap<(String, String), PeerConnection>,
    /// Peers we owe an offer, and peers whose pipeline is ready to make one.
    ///
    /// An offer asked for before webrtcbin has worked out what it is sending
    /// comes back empty, which fails to parse and takes the call with it. The
    /// two halves can arrive in either order, so both are recorded and the
    /// offer is made when they meet.
    offer_wanted: HashSet<(String, String)>,
    negotiation_ready: HashSet<(String, String)>,
    /// Where media comes from.
    ///
    /// A camera can only be opened once, so two clients on one machine cannot
    /// both have it. Test patterns are what make a call testable on a single
    /// host, and in CI where there is no camera at all.
    source: Source,
    /// What to offer. Video is dropped when only audio is wanted.
    media: MediaWanted,
    /// Where media events are funnelled.
    media_tx: mpsc::UnboundedSender<TaggedMediaEvent>,
}

struct ActiveCall {
    call: Call,
    /// The channel or nickname the call belongs to.
    target: String,
    /// Peers ringing us, awaiting an answer.
    ringing: Vec<String>,
    /// Peers whose departure we have already announced.
    ///
    /// A peer who hangs up says so directly, and the server then confirms it.
    /// Both are worth having -- the direct farewell carries a reason, the
    /// server's notice covers a peer who vanished without one -- but only the
    /// first of them is worth telling the user about.
    departed: Vec<String>,
    /// Peers we have already invited.
    ///
    /// The server announces a join to everybody, and that can arrive either
    /// side of the peer's own acceptance. Without this, a second invitation
    /// goes out and the peer rings twice for one call.
    invited: Vec<String>,
}

impl Calls {
    /// Start with a fresh identity.
    ///
    /// A generated-per-run identity means every call reports the key as new.
    /// Persisting it is what makes trust-on-first-use mean anything, and is
    /// the next thing to do here.
    #[must_use]
    pub fn new(media_tx: mpsc::UnboundedSender<TaggedMediaEvent>) -> Self {
        Self {
            identity: Identity::generate(),
            known: KnownPeers::new(),
            privacy: Privacy::Direct,
            nick: String::new(),
            account: None,
            source: Source::Devices { camera: None },
            media: MediaWanted::audio_video(),
            active: HashMap::new(),
            connections: HashMap::new(),
            offer_wanted: HashSet::new(),
            negotiation_ready: HashSet::new(),
            media_tx,
        }
    }

    /// Record the account we authenticated as.
    pub fn set_account(&mut self, account: Option<String>) {
        self.account = account;
    }

    /// Use generated test patterns instead of real capture devices.
    pub fn use_test_media(&mut self) {
        self.source = Source::Test;
        status("calls will use test tones and a test pattern, not your devices");
    }

    /// Use a named camera rather than whichever one the system ranks first.
    pub fn use_camera(&mut self, camera: String) {
        self.source = Source::Devices {
            camera: Some(camera),
        };
    }

    /// Offer audio only, leaving the camera alone.
    pub fn audio_only(&mut self) {
        self.media = MediaWanted {
            audio: true,
            video: false,
            screen: false,
        };
    }

    /// Record our nickname, which signalling binds to.
    pub fn set_nick(&mut self, nick: String) {
        self.nick = nick;
    }

    /// Choose what to disclose when gathering.
    pub fn set_privacy(&mut self, privacy: Privacy) {
        self.privacy = privacy;
        status(&format!(
            "calls will {}",
            match privacy {
                Privacy::Direct => "connect directly; peers will see your IP address",
                Privacy::RelayOnly => "use a relay only; peers will not see your IP address",
            }
        ));
    }

    /// Ask the server to start a call.
    pub fn start(&self, handle: &Handle, target: &str) -> Result<()> {
        if self.account.is_none() {
            warn("you are not logged in: the other side cannot confirm who you are");
        }
        let wanted = if self.media.video {
            "audio,video"
        } else {
            "audio"
        };
        handle.send(
            MessageBuf::new("CALL")
                .param("START")
                .param(target)
                .param(wanted),
        )?;
        status(&format!("calling {target}"));
        Ok(())
    }

    /// Answer whoever is ringing.
    pub fn answer(&mut self, handle: &Handle) -> Result<()> {
        let Some((call_id, peer)) = self.first_ringing() else {
            bail!("nobody is calling");
        };
        let Some(entry) = self.active.get_mut(&call_id) else {
            bail!("that call has gone");
        };

        // Telling the server first: it is what admits us to the call, and
        // until it does a signalling relay would refuse our payloads.
        handle.send(MessageBuf::new("CALL").param("ACCEPT").param(&*call_id))?;

        let outcome = entry.call.accept(&peer)?;
        entry.ringing.retain(|r| r != &peer);
        self.apply(handle, &call_id, outcome)
    }

    /// Refuse whoever is ringing.
    pub fn reject(&mut self, handle: &Handle) -> Result<()> {
        let Some((call_id, peer)) = self.first_ringing() else {
            bail!("nobody is calling");
        };
        handle.send(
            MessageBuf::new("CALL")
                .param("DECLINE")
                .param(&*call_id)
                .trailing("declined"),
        )?;
        if let Some(entry) = self.active.get_mut(&call_id) {
            entry.ringing.retain(|r| r != &peer);
            let _ = entry.call.hang_up(&peer, "declined");
        }
        self.forget_if_empty(&call_id);
        status("declined");
        Ok(())
    }

    /// Leave every call.
    pub fn hang_up(&mut self, handle: &Handle) -> Result<()> {
        if self.active.is_empty() {
            bail!("no call in progress");
        }
        let ids: Vec<String> = self.active.keys().cloned().collect();
        for call_id in ids {
            let peers: Vec<String> = self
                .active
                .get(&call_id)
                .map(|e| e.call.peers().map(str::to_owned).collect())
                .unwrap_or_default();
            for peer in peers {
                if let Some(entry) = self.active.get_mut(&call_id)
                    && let Ok(outcome) = entry.call.hang_up(&peer, "hung up")
                {
                    self.apply(handle, &call_id, outcome)?;
                }
            }
            handle.send(MessageBuf::new("CALL").param("LEAVE").param(&*call_id))?;
            self.close_call(&call_id);
        }
        status("call ended");
        Ok(())
    }

    /// Confirm that a short authentication string matched.
    pub fn verify(&mut self) -> Result<()> {
        let Some(entry) = self.active.values_mut().next() else {
            bail!("no call in progress");
        };
        let peers: Vec<String> = entry.call.peers().map(str::to_owned).collect();
        let mut any = false;
        for peer in peers {
            any |= entry.call.mark_verified(&peer);
        }
        if any {
            status("verified; this key will be remembered");
        } else {
            warn("nothing to verify: the peer is not logged in, so there is no account to remember a key against");
        }
        Ok(())
    }

    /// Handle a `CALL` message from the server.
    pub fn on_call_message(
        &mut self,
        handle: &Handle,
        call_id: &[u8],
        verb: &[u8],
        from: &[u8],
        params: &[Vec<u8>],
    ) -> Result<()> {
        let call_id = String::from_utf8_lossy(call_id).into_owned();
        let peer = String::from_utf8_lossy(from).into_owned();
        let text = |index: usize| -> String {
            params
                .get(index)
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .unwrap_or_default()
        };

        match verb {
            b"STARTED" => {
                let target = text(0);
                status(&format!("call {call_id} started on {target}"));
                self.ensure_call(&call_id, &target);

                // Calling a person invites them straight away. The server's
                // own invitation only tells them somebody is calling; this is
                // the key exchange, without which they have nothing to answer
                // with. A channel call waits for JOINED instead, because
                // there is nobody in particular to invite yet.
                let is_channel = target.starts_with('#') || target.starts_with('&');
                if !is_channel {
                    self.invite_once(handle, &call_id, &target)?;
                }
            }
            b"INVITE" => {
                let target = text(0);
                self.ensure_call(&call_id, &target);
                status(&format!(
                    "{}{peer} is calling{} — /answer or /reject",
                    colour::GREEN,
                    colour::RESET
                ));
            }
            b"JOINED" => {
                self.ensure_call(&call_id, &text(0));
                // The server tells everyone, including the person who joined.
                // Acting on our own arrival would have us invite ourselves.
                if peer == self.nick {
                    return Ok(());
                }
                status(&format!("{peer} joined the call"));

                // Whoever was already in the call invites the newcomer, which
                // builds a mesh without the server brokering it.
                self.invite_once(handle, &call_id, &peer)?;
            }
            b"SIGNAL" => {
                let payload = text(0);
                let Some(entry) = self.active.get_mut(&call_id) else {
                    return Ok(());
                };
                match entry.call.on_signal(&peer, None, &payload) {
                    Ok(outcome) => self.apply(handle, &call_id, outcome)?,
                    // A payload we cannot read is not worth dropping the call
                    // over, but it is worth saying: it means somebody is out
                    // of step, or something rewrote it.
                    Err(error) => warn(&format!("signalling from {peer} was rejected: {error}")),
                }
            }
            b"LEFT" | b"DECLINED" => {
                if self.announce_departure(&call_id, &peer) {
                    status(&format!("{peer} left the call"));
                }
                if let Some(entry) = self.active.get_mut(&call_id) {
                    let _ = entry.call.hang_up(&peer, "left");
                    entry.ringing.retain(|r| r != &peer);
                }
                self.connections.remove(&(call_id.clone(), peer));
                self.forget_if_empty(&call_id);
            }
            _ => {}
        }
        Ok(())
    }

    /// Handle something the media engine reported.
    pub fn on_media(&mut self, handle: &Handle, tagged: TaggedMediaEvent) -> Result<()> {
        let TaggedMediaEvent { peer, event } = tagged;
        let Some(call_id) = self.call_of(&peer) else {
            return Ok(());
        };

        let translated = match event {
            PeerEvent::LocalDescription { kind, sdp } => MediaEvent::LocalDescription {
                peer: peer.clone(),
                kind,
                sdp,
            },
            PeerEvent::IceCandidate {
                mline_index,
                candidate,
            } => MediaEvent::LocalCandidate {
                peer: peer.clone(),
                mline_index,
                candidate,
            },
            PeerEvent::ConnectionState(state) => {
                match state {
                    ConnectionState::Connected => status(&format!(
                        "{}connected to {peer}{}",
                        colour::GREEN,
                        colour::RESET
                    )),
                    ConnectionState::Failed => warn(&format!(
                        "could not reach {peer}: no route worked. Both ends are probably \
                         behind restrictive NATs and no relay is configured"
                    )),
                    _ => {}
                }
                return Ok(());
            }
            PeerEvent::RemoteTrack { kind } => {
                status(&format!("receiving {kind} from {peer}"));
                return Ok(());
            }
            PeerEvent::Error(reason) => MediaEvent::Failed {
                peer: peer.clone(),
                reason,
            },
            PeerEvent::NegotiationNeeded => {
                self.negotiation_ready.insert((call_id.clone(), peer.clone()));
                self.offer_if_ready(&call_id, &peer);
                return Ok(());
            }
            PeerEvent::IceState(_) => return Ok(()),
        };

        let Some(entry) = self.active.get_mut(&call_id) else {
            return Ok(());
        };
        let outcome = entry.call.on_media(translated)?;
        self.apply(handle, &call_id, outcome)
    }

    // --- internals ---------------------------------------------------------

    /// Make the offer once the pipeline is ready and one has been asked for.
    fn offer_if_ready(&mut self, call_id: &str, peer: &str) {
        let key = (call_id.to_owned(), peer.to_owned());
        if !self.offer_wanted.contains(&key) || !self.negotiation_ready.contains(&key) {
            return;
        }
        if let Some(connection) = self.connections.get(&key) {
            connection.create_offer();
            self.offer_wanted.remove(&key);
        }
    }

    /// Whether this peer's departure is news.
    ///
    /// Returns true exactly once per peer per call, so a departure reported
    /// by both the peer and the server is only shown the once.
    fn announce_departure(&mut self, call_id: &str, peer: &str) -> bool {
        let Some(entry) = self.active.get_mut(call_id) else {
            return true;
        };
        if entry.departed.iter().any(|p| p == peer) {
            return false;
        }
        entry.departed.push(peer.to_owned());
        true
    }

    /// Invite a peer, unless we already have.
    fn invite_once(&mut self, handle: &Handle, call_id: &str, peer: &str) -> Result<()> {
        let Some(entry) = self.active.get_mut(call_id) else {
            return Ok(());
        };
        if entry.invited.iter().any(|p| p == peer) {
            return Ok(());
        }
        entry.invited.push(peer.to_owned());
        let outcome = entry.call.invite(peer)?;
        self.apply(handle, call_id, outcome)
    }

    fn ensure_call(&mut self, call_id: &str, target: &str) {
        if self.active.contains_key(call_id) {
            return;
        }
        let call = Call::new(
            call_id.as_bytes().to_vec(),
            self.nick.clone(),
            Identity::from_secret(self.identity.to_secret()),
            self.known.clone(),
        )
        .with_account(self.account.clone())
        .with_privacy(self.privacy)
        .with_media(self.media);

        self.active.insert(
            call_id.to_owned(),
            ActiveCall {
                call,
                target: target.to_owned(),
                ringing: Vec::new(),
                departed: Vec::new(),
                invited: Vec::new(),
            },
        );
    }

    /// Carry out what the state machine decided.
    fn apply(&mut self, handle: &Handle, call_id: &str, outcome: kestrel_call::Outcome) -> Result<()> {
        for action in outcome.actions {
            match action {
                Action::Signal { peer, payload } => {
                    handle.send(
                        MessageBuf::new("CALL")
                            .param("SIGNAL")
                            .param(call_id)
                            .param(&*peer)
                            .trailing(payload),
                    )?;
                }
                Action::OpenPeer { peer, media } => self.open_peer(call_id, &peer, media),
                Action::CreateOffer { peer } => {
                    self.offer_wanted.insert((call_id.to_owned(), peer.clone()));
                    self.offer_if_ready(call_id, &peer);
                }
                Action::CreateAnswer { peer } => {
                    if let Some(connection) = self.connections.get(&(call_id.to_owned(), peer)) {
                        connection.create_answer();
                    }
                }
                Action::SetRemoteDescription { peer, kind, sdp } => {
                    if let Some(connection) = self.connections.get(&(call_id.to_owned(), peer))
                        && let Err(error) = connection.set_remote_description(&kind, &sdp)
                    {
                        warn(&format!("could not apply a description: {error}"));
                    }
                }
                Action::AddCandidate {
                    peer,
                    mline_index,
                    candidate,
                } => {
                    if let Some(connection) = self.connections.get(&(call_id.to_owned(), peer)) {
                        connection.add_ice_candidate(mline_index, &candidate);
                    }
                }
                Action::ClosePeer { peer } => {
                    self.connections.remove(&(call_id.to_owned(), peer));
                }
            }
        }

        for event in outcome.events {
            self.show(call_id, event);
        }
        Ok(())
    }

    fn show(&mut self, call_id: &str, event: Event) {
        match event {
            Event::Ringing { peer, account, .. } => {
                let who = account.unwrap_or_else(|| format!("{peer} (not logged in)"));
                status(&format!(
                    "{}{who} is calling{} — /answer or /reject",
                    colour::GREEN,
                    colour::RESET
                ));
                if let Some(entry) = self.active.get_mut(call_id) {
                    entry.ringing.push(peer);
                }
            }
            Event::Verify { peer, sas, trust } => {
                match trust {
                    Trust::Changed { .. } => warn(&format!(
                        "{peer}'s key has CHANGED. Either they have a new device, or somebody \
                         is impersonating them. Do not continue until you have checked."
                    )),
                    Trust::Verified => status(&format!("{peer}'s key was verified previously")),
                    Trust::New | Trust::Known => {}
                }
                // Printed rather than logged: this phrase is the only check
                // that survives a hostile server, and it is worthless unless
                // somebody actually reads it aloud.
                println!(
                    "{}== say this aloud to {peer}: {}{}{} ==  (/verify once it matches)",
                    colour::CYAN,
                    colour::BOLD,
                    sas.phrase(),
                    colour::RESET
                );
            }
            Event::PeerGone { peer, reason } => {
                if self.announce_departure(call_id, &peer) {
                    status(&format!("{peer} left the call ({reason})"));
                }
                self.connections.remove(&(call_id.to_owned(), peer));
            }
            Event::PeerFailed { peer, reason } => {
                warn(&format!("the connection to {peer} failed: {reason}"));
            }
        }
    }

    fn open_peer(&mut self, call_id: &str, peer: &str, media: MediaWanted) {
        let key = (call_id.to_owned(), peer.to_owned());
        if self.connections.contains_key(&key) {
            return;
        }
        let sending = Sending {
            audio: media.audio,
            video: media.video,
        };
        let name = format!("{call_id}-{peer}");

        if matches!(self.source, Source::Devices { .. }) {
            // Said before it happens, not after. On Windows the default camera
            // can be a paired phone, so opening it makes that phone ring.
            status(&format!(
                "opening your {} — pass --test-media to use test patterns instead",
                if sending.video {
                    "microphone and camera"
                } else {
                    "microphone"
                }
            ));
        }

        match PeerConnection::new(&name, sending, &self.source, None) {
            Ok((connection, mut events)) => {
                // Media events arrive on GStreamer's threads; tagging and
                // forwarding them is all that happens there.
                let tx = self.media_tx.clone();
                let who = peer.to_owned();
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        if tx
                            .send(TaggedMediaEvent {
                                peer: who.clone(),
                                event,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                self.connections.insert(key, connection);
            }
            Err(error) => warn(&format!("could not open the microphone or camera: {error}")),
        }
    }

    fn first_ringing(&self) -> Option<(String, String)> {
        self.active.iter().find_map(|(id, entry)| {
            entry
                .ringing
                .first()
                .map(|peer| (id.clone(), peer.clone()))
        })
    }

    fn call_of(&self, peer: &str) -> Option<String> {
        self.connections
            .keys()
            .find(|(_, who)| who == peer)
            .map(|(call_id, _)| call_id.clone())
    }

    fn forget_if_empty(&mut self, call_id: &str) {
        let empty = self
            .active
            .get(call_id)
            .is_some_and(|e| e.call.peers().next().is_none() && e.ringing.is_empty());
        if empty {
            self.close_call(call_id);
        }
    }

    fn close_call(&mut self, call_id: &str) {
        if let Some(entry) = self.active.remove(call_id) {
            // Keep what was learned about identities for the next call.
            self.known = entry.call.known_peers().clone();
            let _ = entry.target;
        }
        self.connections.retain(|(id, _), _| id != call_id);
    }
}
