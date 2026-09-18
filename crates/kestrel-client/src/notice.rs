//! What a client should show.
//!
//! The call machinery has plenty to say -- who is ringing, whose key changed,
//! the phrase two people read aloud to each other -- and no business deciding
//! how any of it looks. This is the whole of the vocabulary between them.

/// How much attention a notice deserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something happened.
    Info,
    /// Something went wrong, or is worth being careful about.
    Warning,
    /// Something the user has to read rather than merely be told.
    ///
    /// Reserved for the short authentication string and an incoming call: one
    /// is the only defence against a hostile server and is worthless unread,
    /// and the other is a question waiting for an answer.
    Highlight,
}

/// One thing worth showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// How much attention it deserves.
    pub level: Level,
    /// What to show.
    pub text: String,
    /// Which conversation it belongs to, where there is one.
    ///
    /// A channel or a nickname. An interface with somewhere to put it should;
    /// a call with somebody belongs in the conversation with them rather than
    /// in a general log. `None` means there is no such place -- the notice
    /// concerns no particular call.
    pub target: Option<String>,
}

impl Notice {
    /// A notice belonging to no particular conversation.
    #[must_use]
    pub fn new(level: Level, text: impl Into<String>) -> Self {
        Self {
            level,
            text: text.into(),
            target: None,
        }
    }

    /// A notice belonging to one conversation.
    #[must_use]
    pub fn in_target(level: Level, text: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            level,
            text: text.into(),
            target: Some(target.into()),
        }
    }
}
