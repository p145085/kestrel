//! What a session reports to whatever is driving it.
//!
//! Events are the client's vocabulary: a UI renders them, a headless client
//! prints them, and a test asserts on them. Keeping them as data rather than
//! callbacks is what lets the GTK client and the terminal one show the same
//! thing without sharing any drawing code.

use kestrel_proto::MessageBuf;

/// Where a message was addressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A channel.
    Channel(Vec<u8>),
    /// Us, directly.
    Direct(Vec<u8>),
}

impl Target {
    /// The name, whichever kind of target this is.
    #[must_use]
    pub fn name(&self) -> &[u8] {
        match self {
            Self::Channel(name) | Self::Direct(name) => name,
        }
    }

    /// Whether this is a channel.
    #[must_use]
    pub fn is_channel(&self) -> bool {
        matches!(self, Self::Channel(_))
    }
}

/// Who sent something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    /// The nickname, or the server's name.
    pub nick: Vec<u8>,
    /// The full `nick!user@host` mask, when there was one.
    pub mask: Option<Vec<u8>>,
    /// The services account, when the server told us.
    pub account: Option<Vec<u8>>,
    /// Whether this came from the server rather than a user.
    pub is_server: bool,
}

/// Whether a message was a `PRIVMSG` or a `NOTICE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// An ordinary message.
    Privmsg,
    /// A notice, which must never be replied to automatically.
    Notice,
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The server sent `ERROR`.
    ServerError(String),
    /// Registration could not complete.
    RegistrationFailed(String),
}

/// Something that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Registration completed; this is our nickname.
    Registered {
        /// The nickname the server settled on, which may not be the one asked for.
        nick: Vec<u8>,
    },
    /// The capabilities the server agreed to.
    CapsEnabled(Vec<String>),
    /// SASL succeeded.
    LoggedIn {
        /// The account we are now recognised as.
        account: Vec<u8>,
    },
    /// SASL failed. The session continues unauthenticated.
    LoginFailed {
        /// What the server said.
        reason: String,
    },
    /// A message arrived.
    Message {
        /// Where it was addressed.
        target: Target,
        /// Who sent it.
        from: Sender,
        /// The text.
        text: Vec<u8>,
        /// Whether it was a notice.
        kind: MessageKind,
        /// `server-time`, when the server tagged it.
        time: Option<String>,
    },
    /// A tags-only message, such as a typing notification.
    TagMessage {
        /// Where it was addressed.
        target: Target,
        /// Who sent it.
        from: Sender,
        /// The client-only tags, unescaped.
        tags: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// Somebody joined a channel.
    Joined {
        /// The channel.
        channel: Vec<u8>,
        /// Who joined.
        who: Sender,
        /// Whether that was us.
        is_self: bool,
    },
    /// Somebody left a channel.
    Parted {
        /// The channel.
        channel: Vec<u8>,
        /// Who left.
        who: Sender,
        /// Their parting message.
        reason: Option<Vec<u8>>,
        /// Whether that was us.
        is_self: bool,
    },
    /// Somebody disconnected.
    Quit {
        /// Who left.
        who: Sender,
        /// Their parting message.
        reason: Option<Vec<u8>>,
        /// Channels we shared with them, so a UI knows where to show it.
        channels: Vec<Vec<u8>>,
    },
    /// Somebody was removed from a channel.
    Kicked {
        /// The channel.
        channel: Vec<u8>,
        /// Who was removed.
        who: Vec<u8>,
        /// Who removed them.
        by: Sender,
        /// The stated reason.
        reason: Option<Vec<u8>>,
        /// Whether that was us.
        is_self: bool,
    },
    /// Somebody changed nickname.
    NickChanged {
        /// Their previous nickname.
        old: Vec<u8>,
        /// Their new nickname.
        new: Vec<u8>,
        /// Channels we share with them.
        channels: Vec<Vec<u8>>,
        /// Whether that was us.
        is_self: bool,
    },
    /// A channel topic was set or reported.
    Topic {
        /// The channel.
        channel: Vec<u8>,
        /// The topic, or `None` when there is none.
        topic: Option<Vec<u8>>,
        /// Who set it, when the server said.
        setter: Option<Vec<u8>>,
    },
    /// A channel's member list changed.
    ///
    /// Carries no roster: the session holds it, and copying it into every
    /// event would make a large channel expensive to keep up to date.
    RosterChanged {
        /// The channel whose roster moved.
        channel: Vec<u8>,
    },
    /// Modes changed on a channel or on us.
    ModeChanged {
        /// What the modes were set on.
        target: Vec<u8>,
        /// The mode string, such as `+nt`.
        spec: Vec<u8>,
        /// Parameters consumed by those modes.
        params: Vec<Vec<u8>>,
        /// Who changed them.
        by: Sender,
    },
    /// Somebody went away or came back.
    AwayChanged {
        /// Whose status changed.
        nick: Vec<u8>,
        /// Their away message, or `None` if they are back.
        message: Option<Vec<u8>>,
    },
    /// We were invited to a channel.
    Invited {
        /// The channel.
        channel: Vec<u8>,
        /// Who invited us.
        by: Sender,
    },
    /// A numeric reply the session did not consume itself.
    ///
    /// Everything from `WHOIS` output to error replies arrives here, so a
    /// client can show them without the session needing to know each one.
    Numeric {
        /// The three-digit code.
        code: u16,
        /// The parameters, minus the leading nickname.
        params: Vec<Vec<u8>>,
        /// The trailing text, when there was one.
        text: Option<Vec<u8>>,
    },
    /// A message the session did not recognise, passed through unchanged.
    Raw(MessageBuf),
    /// The session ended.
    Ended(Ended),
}

#[cfg(test)]
mod tests {
    use super::Target;

    #[test]
    fn a_target_reports_its_name_either_way() {
        assert_eq!(Target::Channel(b"#chan".to_vec()).name(), b"#chan");
        assert_eq!(Target::Direct(b"alice".to_vec()).name(), b"alice");
    }

    #[test]
    fn only_channels_are_channels() {
        assert!(Target::Channel(b"#chan".to_vec()).is_channel());
        assert!(!Target::Direct(b"alice".to_vec()).is_channel());
    }
}
