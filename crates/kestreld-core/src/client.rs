//! Per-connection client state.

use std::collections::HashSet;

/// Identifies one connected client for the lifetime of the server process.
///
/// Ids are never reused, so a stale id from a disconnected client resolves to
/// nothing rather than to whoever connected next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub(crate) u64);

impl ClientId {
    /// The underlying value, for logging.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client#{}", self.0)
    }
}

/// How far through registration a connection has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationState {
    /// Still collecting `NICK`, `USER`, and possibly `PASS` and capabilities.
    Pending,
    /// Registered; the welcome burst has been sent.
    Registered,
}

/// One connected client.
#[derive(Debug, Clone)]
pub struct Client {
    pub(crate) id: ClientId,
    pub(crate) state: RegistrationState,
    /// Hostname to show in this client's mask.
    pub(crate) host: Vec<u8>,
    pub(crate) nick: Option<Vec<u8>>,
    pub(crate) user: Option<Vec<u8>>,
    pub(crate) realname: Vec<u8>,
    /// Whether an acceptable `PASS` has been supplied, when one is required.
    pub(crate) password_ok: bool,
    /// Whether the client asked for capabilities and has not sent `CAP END`.
    pub(crate) cap_negotiating: bool,
    /// Folded names of the channels this client is in.
    pub(crate) channels: HashSet<Vec<u8>>,
    /// Away message, if the client is marked away.
    pub(crate) away: Option<Vec<u8>>,
}

impl Client {
    pub(crate) fn new(id: ClientId, host: Vec<u8>) -> Self {
        Self {
            id,
            state: RegistrationState::Pending,
            host,
            nick: None,
            user: None,
            realname: Vec::new(),
            password_ok: false,
            cap_negotiating: false,
            channels: HashSet::new(),
            away: None,
        }
    }

    /// This client's id.
    #[must_use]
    pub fn id(&self) -> ClientId {
        self.id
    }

    /// Whether registration has completed.
    #[must_use]
    pub fn is_registered(&self) -> bool {
        self.state == RegistrationState::Registered
    }

    /// The current nickname, or `*` before one has been chosen.
    ///
    /// Numerics need a target parameter even when the client has not yet sent
    /// `NICK`, and `*` is the conventional placeholder.
    #[must_use]
    pub fn nick_or_star(&self) -> &[u8] {
        self.nick.as_deref().unwrap_or(b"*")
    }

    /// The current nickname, if one has been chosen.
    #[must_use]
    pub fn nick(&self) -> Option<&[u8]> {
        self.nick.as_deref()
    }

    /// The username, if `USER` has been sent.
    #[must_use]
    pub fn user(&self) -> Option<&[u8]> {
        self.user.as_deref()
    }

    /// The realname from `USER`.
    #[must_use]
    pub fn realname(&self) -> &[u8] {
        &self.realname
    }

    /// The hostname shown in this client's mask.
    #[must_use]
    pub fn host(&self) -> &[u8] {
        &self.host
    }

    /// The away message, if the client is away.
    #[must_use]
    pub fn away(&self) -> Option<&[u8]> {
        self.away.as_deref()
    }

    /// Folded names of the channels this client is in.
    #[must_use]
    pub fn channels(&self) -> &HashSet<Vec<u8>> {
        &self.channels
    }

    /// The full `nick!user@host` mask, used as the source of messages this
    /// client originates.
    #[must_use]
    pub fn mask(&self) -> Vec<u8> {
        let mut mask = Vec::with_capacity(32);
        mask.extend_from_slice(self.nick_or_star());
        mask.push(b'!');
        mask.extend_from_slice(self.user.as_deref().unwrap_or(b"*"));
        mask.push(b'@');
        mask.extend_from_slice(&self.host);
        mask
    }

    /// Whether everything registration requires has now been supplied.
    pub(crate) fn can_register(&self, password_required: bool) -> bool {
        self.state == RegistrationState::Pending
            && self.nick.is_some()
            && self.user.is_some()
            // Capability negotiation holds registration open until CAP END,
            // so the client can finish negotiating before it is welcomed.
            && !self.cap_negotiating
            && (!password_required || self.password_ok)
    }
}
