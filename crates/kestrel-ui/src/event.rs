//! What crosses between the interface and the connection.
//!
//! Two rules shape everything here. Nothing in [`AppEvent`] may hold a GTK or
//! GDK type, because those are `!Send` and these values cross a thread
//! boundary. And nothing here may hold a `webrtcbin`, a socket or a session:
//! the interface is told what happened, never handed the thing it happened to.

/// Which buffer something belongs to.
///
/// A channel is named by its channel name, a private conversation by the other
/// party's nickname, and the server itself by the empty string -- which cannot
/// collide with either, since neither may be empty.
pub type BufferId = String;

/// The buffer that holds anything not tied to a channel or a conversation.
pub const SERVER_BUFFER: &str = "";

/// How a line should read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Somebody said something.
    Message,
    /// Somebody did something, in the third person.
    Action,
    /// A notice, which is never replied to automatically.
    Notice,
    /// Something happened: a join, a topic, a mode change.
    Status,
    /// Something went wrong.
    Error,
    /// What we sent ourselves.
    Own,
}

/// One line in a buffer.
#[derive(Debug, Clone)]
pub struct Line {
    /// How it should read.
    pub kind: LineKind,
    /// Who it came from, where that makes sense.
    pub who: Option<String>,
    /// The text.
    pub text: String,
}

impl Line {
    /// A line somebody said.
    pub fn message(who: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind: LineKind::Message,
            who: Some(who.into()),
            text: text.into(),
        }
    }

    /// A line we said.
    pub fn own(who: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind: LineKind::Own,
            who: Some(who.into()),
            text: text.into(),
        }
    }

    /// Something that happened, with nobody to attribute it to.
    pub fn status(text: impl Into<String>) -> Self {
        Self {
            kind: LineKind::Status,
            who: None,
            text: text.into(),
        }
    }

    /// Something that went wrong.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            kind: LineKind::Error,
            who: None,
            text: text.into(),
        }
    }

    /// Somebody acting, in the third person.
    pub fn action(who: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind: LineKind::Action,
            who: Some(who.into()),
            text: text.into(),
        }
    }

    /// A notice.
    pub fn notice(who: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind: LineKind::Notice,
            who: Some(who.into()),
            text: text.into(),
        }
    }
}

/// Something the interface should show.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// A connection attempt has begun.
    Connecting {
        /// Where to.
        server: String,
    },
    /// Registration completed under this nickname.
    Registered {
        /// What the server settled on, which need not be what was asked for.
        nick: String,
    },
    /// Our own nickname changed.
    NickChanged {
        /// The new one.
        nick: String,
    },
    /// Show a line in a buffer, creating the buffer if it is new.
    Line {
        /// Which buffer.
        buffer: BufferId,
        /// What to show.
        line: Line,
    },
    /// Open a buffer and bring it forward.
    OpenBuffer {
        /// Which buffer.
        buffer: BufferId,
    },
    /// Close a buffer, because we left the channel it belonged to.
    CloseBuffer {
        /// Which buffer.
        buffer: BufferId,
    },
    /// Replace a channel's member list.
    Roster {
        /// Which channel.
        buffer: BufferId,
        /// Members, in the order the server gave them.
        members: Vec<String>,
    },
    /// A channel's topic.
    Topic {
        /// Which channel.
        buffer: BufferId,
        /// The topic, or empty when there is none.
        topic: String,
    },
    /// What the server calls itself, and what it runs.
    ///
    /// Worth showing: "server" names no server, and which of several windows
    /// is which matters as soon as there is more than one.
    ServerInfo {
        /// The server's own name for itself.
        name: String,
        /// The software it runs.
        version: String,
    },
    /// The connection ended.
    Disconnected {
        /// Why.
        reason: String,
    },
}

/// Something the interface wants done.
#[derive(Debug, Clone)]
pub enum UiCommand {
    /// The user typed something into a buffer.
    ///
    /// Slash commands are parsed on the connection side rather than here, so
    /// that the parsing is testable without a display.
    Input {
        /// Which buffer it was typed into.
        buffer: BufferId,
        /// What was typed.
        text: String,
    },
    /// Leave, and close the connection.
    Quit {
        /// The parting message.
        reason: String,
    },
}
