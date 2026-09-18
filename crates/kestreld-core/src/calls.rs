//! The call registry: who is in which call.
//!
//! The server routes signalling and nothing else. It never sees media, and the
//! payloads it relays are sealed to a specific recipient, so this holds only
//! membership and state — the things a client needs to discover a call and be
//! routed into it.

use std::collections::HashMap;

use crate::client::ClientId;

/// Identifies one call for the lifetime of the server process.
///
/// Opaque by contract: clients must not parse it. Ids are never reused, so a
/// stale id resolves to nothing rather than to whichever call came next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallId(pub(crate) u64);

impl CallId {
    /// The underlying value, for logging.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// The form that goes on the wire.
    #[must_use]
    pub fn to_wire(self) -> Vec<u8> {
        format!("c{}", self.0).into_bytes()
    }

    /// Parse a wire form back, if it is one.
    #[must_use]
    pub fn parse(raw: &[u8]) -> Option<Self> {
        let digits = raw.strip_prefix(b"c")?;
        std::str::from_utf8(digits).ok()?.parse().ok().map(Self)
    }
}

impl std::fmt::Display for CallId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "c{}", self.0)
    }
}

/// What a call carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Media {
    /// Audio.
    pub audio: bool,
    /// Video.
    pub video: bool,
    /// A shared screen.
    pub screen: bool,
}

impl Media {
    /// Audio and video, the default for a call nobody has narrowed.
    #[must_use]
    pub fn audio_video() -> Self {
        Self {
            audio: true,
            video: true,
            screen: false,
        }
    }

    /// Parse a comma-separated list such as `audio,video`.
    ///
    /// Unknown kinds are ignored rather than rejected, so a future client
    /// asking for something this server has not heard of still gets a call
    /// with the parts it does understand.
    #[must_use]
    pub fn parse(raw: &[u8]) -> Self {
        let mut media = Self::default();
        for part in raw.split(|&b| b == b',') {
            match part.to_ascii_lowercase().as_slice() {
                b"audio" => media.audio = true,
                b"video" => media.video = true,
                b"screen" => media.screen = true,
                _ => {}
            }
        }
        if media == Self::default() {
            // A call carrying nothing is not a call.
            return Self::audio_video();
        }
        media
    }

    /// The form that goes on the wire.
    #[must_use]
    pub fn to_wire(self) -> Vec<u8> {
        let mut parts: Vec<&str> = Vec::with_capacity(3);
        if self.audio {
            parts.push("audio");
        }
        if self.video {
            parts.push("video");
        }
        if self.screen {
            parts.push("screen");
        }
        parts.join(",").into_bytes()
    }
}

/// Somebody in a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Participant {
    /// Which connection.
    pub client: ClientId,
    /// Whether they have accepted, as opposed to merely being invited.
    pub accepted: bool,
}

/// One call.
#[derive(Debug, Clone)]
pub struct Call {
    pub(crate) id: CallId,
    /// The channel or nickname the call belongs to, in its original case.
    pub(crate) target: Vec<u8>,
    /// Whether `target` is a channel.
    pub(crate) in_channel: bool,
    pub(crate) media: Media,
    pub(crate) started_at: u64,
    /// Everyone in or invited to the call.
    pub(crate) participants: Vec<Participant>,
}

impl Call {
    pub(crate) fn new(
        id: CallId,
        target: Vec<u8>,
        in_channel: bool,
        media: Media,
        started_at: u64,
    ) -> Self {
        Self {
            id,
            target,
            in_channel,
            media,
            started_at,
            participants: Vec::new(),
        }
    }

    /// The call's identifier.
    #[must_use]
    pub fn id(&self) -> CallId {
        self.id
    }

    /// The channel or nickname this call belongs to.
    #[must_use]
    pub fn target(&self) -> &[u8] {
        &self.target
    }

    /// Whether the call belongs to a channel rather than a nickname.
    #[must_use]
    pub fn is_in_channel(&self) -> bool {
        self.in_channel
    }

    /// What the call carries.
    #[must_use]
    pub fn media(&self) -> Media {
        self.media
    }

    /// When it started, in Unix seconds.
    #[must_use]
    pub fn started_at(&self) -> u64 {
        self.started_at
    }

    /// Everyone who has accepted.
    pub fn accepted(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.participants
            .iter()
            .filter(|p| p.accepted)
            .map(|p| p.client)
    }

    /// Everyone in or invited to the call.
    pub fn everyone(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.participants.iter().map(|p| p.client)
    }

    /// How many have accepted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.participants.iter().filter(|p| p.accepted).count()
    }

    /// Whether nobody is left.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.participants.is_empty()
    }

    /// Whether `client` has accepted.
    #[must_use]
    pub fn has_accepted(&self, client: ClientId) -> bool {
        self.participants
            .iter()
            .any(|p| p.client == client && p.accepted)
    }

    /// Whether `client` is in or invited to the call.
    #[must_use]
    pub fn contains(&self, client: ClientId) -> bool {
        self.participants.iter().any(|p| p.client == client)
    }

    pub(crate) fn add(&mut self, client: ClientId, accepted: bool) {
        if let Some(existing) = self.participants.iter_mut().find(|p| p.client == client) {
            // An invitation followed by an accept promotes, never demotes: a
            // second invitation must not un-accept somebody already in a call.
            existing.accepted |= accepted;
            return;
        }
        self.participants.push(Participant { client, accepted });
    }

    pub(crate) fn remove(&mut self, client: ClientId) -> bool {
        let before = self.participants.len();
        self.participants.retain(|p| p.client != client);
        self.participants.len() != before
    }
}

/// Every call the server knows about.
#[derive(Debug, Default)]
pub struct CallRegistry {
    calls: HashMap<CallId, Call>,
    /// Folded target to call, so a channel has at most one call.
    by_target: HashMap<Vec<u8>, CallId>,
    next_id: u64,
}

impl CallRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: 1,
            ..Self::default()
        }
    }

    /// How many calls are in progress.
    #[must_use]
    pub fn len(&self) -> usize {
        self.calls.len()
    }

    /// Whether no calls are in progress.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Look up a call.
    #[must_use]
    pub fn get(&self, id: CallId) -> Option<&Call> {
        self.calls.get(&id)
    }

    /// The call belonging to a folded target, if there is one.
    #[must_use]
    pub fn by_target(&self, folded_target: &[u8]) -> Option<&Call> {
        self.by_target
            .get(folded_target)
            .and_then(|id| self.calls.get(id))
    }

    /// Every call, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = &Call> {
        self.calls.values()
    }

    /// Create a call for a target that has none.
    pub(crate) fn create(
        &mut self,
        folded_target: &[u8],
        target: Vec<u8>,
        in_channel: bool,
        media: Media,
        now: u64,
    ) -> CallId {
        let id = CallId(self.next_id);
        self.next_id += 1;
        self.calls
            .insert(id, Call::new(id, target, in_channel, media, now));
        self.by_target.insert(folded_target.to_vec(), id);
        id
    }

    pub(crate) fn get_mut(&mut self, id: CallId) -> Option<&mut Call> {
        self.calls.get_mut(&id)
    }

    /// Remove a client from every call, reporting which ones they were in.
    ///
    /// Used when a client quits or loses the channel a call belongs to. A
    /// participant who can no longer see the channel must not stay in its call.
    pub(crate) fn remove_client(&mut self, client: ClientId) -> Vec<CallId> {
        let mut affected = Vec::new();
        for (id, call) in &mut self.calls {
            if call.remove(client) {
                affected.push(*id);
            }
        }
        affected
    }

    /// Drop a call that has nobody left in it.
    pub(crate) fn drop_if_empty(&mut self, id: CallId) -> bool {
        let Some(call) = self.calls.get(&id) else {
            return false;
        };
        if !call.is_empty() {
            return false;
        }
        self.by_target.retain(|_, existing| *existing != id);
        self.calls.remove(&id);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{CallId, CallRegistry, Media};
    use crate::client::ClientId;

    #[test]
    fn call_ids_round_trip_through_the_wire_form() {
        let id = CallId(42);
        assert_eq!(id.to_wire(), b"c42");
        assert_eq!(CallId::parse(b"c42"), Some(id));
    }

    #[test]
    fn a_malformed_call_id_is_refused_rather_than_guessed() {
        for bad in [&b""[..], b"42", b"cabc", b"c", b"xc42"] {
            assert_eq!(CallId::parse(bad), None, "should refuse {bad:?}");
        }
    }

    #[test]
    fn media_round_trips() {
        let media = Media::parse(b"audio,video,screen");
        assert_eq!(media.to_wire(), b"audio,video,screen");
        assert_eq!(Media::parse(b"audio").to_wire(), b"audio");
    }

    #[test]
    fn unknown_media_kinds_are_ignored_not_fatal() {
        // A future client asking for something we have not heard of should
        // still get a call with the parts we do understand.
        let media = Media::parse(b"audio,hologram");
        assert!(media.audio);
        assert!(!media.video);
    }

    #[test]
    fn a_call_carrying_nothing_falls_back_to_audio_and_video() {
        assert_eq!(Media::parse(b""), Media::audio_video());
        assert_eq!(Media::parse(b"hologram"), Media::audio_video());
    }

    #[test]
    fn a_target_has_at_most_one_call() {
        let mut registry = CallRegistry::new();
        let id = registry.create(b"#chan", b"#chan".to_vec(), true, Media::audio_video(), 0);
        assert_eq!(registry.by_target(b"#chan").map(super::Call::id), Some(id));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn an_invitation_followed_by_an_accept_promotes() {
        let mut registry = CallRegistry::new();
        let id = registry.create(b"#chan", b"#chan".to_vec(), true, Media::audio_video(), 0);
        let call = registry.get_mut(id).unwrap();

        call.add(ClientId(1), false);
        assert!(call.contains(ClientId(1)));
        assert!(!call.has_accepted(ClientId(1)));
        assert_eq!(call.len(), 0, "an invitation is not attendance");

        call.add(ClientId(1), true);
        assert!(call.has_accepted(ClientId(1)));
        assert_eq!(call.len(), 1);
    }

    #[test]
    fn a_second_invitation_does_not_un_accept_somebody() {
        let mut registry = CallRegistry::new();
        let id = registry.create(b"#chan", b"#chan".to_vec(), true, Media::audio_video(), 0);
        let call = registry.get_mut(id).unwrap();

        call.add(ClientId(1), true);
        call.add(ClientId(1), false);
        assert!(call.has_accepted(ClientId(1)));
    }

    #[test]
    fn a_call_with_nobody_left_is_dropped() {
        let mut registry = CallRegistry::new();
        let id = registry.create(b"#chan", b"#chan".to_vec(), true, Media::audio_video(), 0);
        registry.get_mut(id).unwrap().add(ClientId(1), true);

        assert!(!registry.drop_if_empty(id), "somebody is still in it");
        registry.get_mut(id).unwrap().remove(ClientId(1));
        assert!(registry.drop_if_empty(id));

        assert!(registry.is_empty());
        assert!(
            registry.by_target(b"#chan").is_none(),
            "the target should be free for a new call"
        );
    }

    #[test]
    fn removing_a_client_reports_every_call_they_were_in() {
        let mut registry = CallRegistry::new();
        let first = registry.create(b"#one", b"#one".to_vec(), true, Media::audio_video(), 0);
        let second = registry.create(b"#two", b"#two".to_vec(), true, Media::audio_video(), 0);
        registry.get_mut(first).unwrap().add(ClientId(1), true);
        registry.get_mut(second).unwrap().add(ClientId(1), true);
        registry.get_mut(second).unwrap().add(ClientId(2), true);

        let mut affected = registry.remove_client(ClientId(1));
        affected.sort();
        assert_eq!(affected, vec![first, second]);
        assert_eq!(registry.get(second).unwrap().len(), 1);
    }

    #[test]
    fn ids_are_never_reused() {
        let mut registry = CallRegistry::new();
        let first = registry.create(b"#chan", b"#chan".to_vec(), true, Media::audio_video(), 0);
        registry.drop_if_empty(first);
        let second = registry.create(b"#chan", b"#chan".to_vec(), true, Media::audio_video(), 0);
        assert_ne!(first, second, "a stale id must not resolve to a new call");
    }
}
