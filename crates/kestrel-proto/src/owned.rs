//! Owned messages, for code that builds a message to send.
//!
//! [`Message`] borrows from the buffer it was parsed out of, which is right for
//! the receive path but useless for the send path: a server replying to a
//! client is assembling bytes that exist nowhere yet. [`MessageBuf`] owns its
//! contents, and borrows itself as a [`Message`] to reuse one serialiser rather
//! than maintaining two that could disagree.

use crate::error::SerializeError;
use crate::message::Message;
use crate::numeric;
use crate::source::Source;
use crate::tags;

/// An owned, buildable IRC message.
///
/// ```
/// use kestrel_proto::{MessageBuf, numeric};
///
/// let reply = MessageBuf::numeric(numeric::RPL_WELCOME)
///     .source("irc.example.org")
///     .param("nick")
///     .trailing("Welcome to the network");
///
/// assert_eq!(
///     reply.to_vec()?,
///     b":irc.example.org 001 nick :Welcome to the network\r\n",
/// );
/// # Ok::<(), kestrel_proto::SerializeError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageBuf {
    tags: Vec<(Vec<u8>, Vec<u8>)>,
    source: Option<Vec<u8>>,
    command: Vec<u8>,
    params: Vec<Vec<u8>>,
    force_trailing: bool,
}

impl MessageBuf {
    /// Start building a message with the given command.
    #[must_use]
    pub fn new(command: impl Into<Vec<u8>>) -> Self {
        Self {
            command: command.into(),
            ..Self::default()
        }
    }

    /// Start building a numeric reply, written as three digits.
    #[must_use]
    pub fn numeric(code: u16) -> Self {
        Self::new(numeric::to_wire(code).to_vec())
    }

    /// Set the source.
    #[must_use]
    pub fn source(mut self, source: impl Into<Vec<u8>>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Attach a tag, escaping the value for the wire.
    #[must_use]
    pub fn tag(mut self, key: impl Into<Vec<u8>>, value: impl AsRef<[u8]>) -> Self {
        self.tags
            .push((key.into(), tags::escape(value.as_ref()).into_owned()));
        self
    }

    /// Append a parameter.
    #[must_use]
    pub fn param(mut self, param: impl Into<Vec<u8>>) -> Self {
        self.params.push(param.into());
        self
    }

    /// Append each parameter of an iterator.
    #[must_use]
    pub fn params<I, P>(mut self, params: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<Vec<u8>>,
    {
        self.params.extend(params.into_iter().map(Into::into));
        self
    }

    /// Append a final parameter written in trailing form, prefixed with `:`.
    ///
    /// Use this for free text — a message body, a quit reason, a topic — where
    /// the content is not known in advance and every implementation expects to
    /// see the trailing form.
    #[must_use]
    pub fn trailing(mut self, param: impl Into<Vec<u8>>) -> Self {
        self.params.push(param.into());
        self.force_trailing = true;
        self
    }

    /// Borrow as a [`Message`].
    #[must_use]
    pub fn as_message(&self) -> Message<'_> {
        let mut msg = Message::new(&self.command);
        for (key, raw_value) in &self.tags {
            msg = msg.with_raw_tag(key, raw_value);
        }
        if let Some(source) = &self.source {
            msg = msg.with_source(Source::parse(source));
        }
        let last = self.params.len().saturating_sub(1);
        for (i, param) in self.params.iter().enumerate() {
            msg = if i == last && self.force_trailing {
                msg.with_trailing_param(param)
            } else {
                msg.with_param(param)
            };
        }
        msg
    }

    /// Write the message to `out`, terminated by CRLF.
    pub fn write_to(&self, out: &mut Vec<u8>) -> Result<(), SerializeError> {
        self.as_message().write_to(out)
    }

    /// Serialise to a fresh buffer.
    pub fn to_vec(&self) -> Result<Vec<u8>, SerializeError> {
        self.as_message().to_vec()
    }
}

impl<'a> From<&Message<'a>> for MessageBuf {
    fn from(msg: &Message<'a>) -> Self {
        Self {
            tags: msg
                .tags()
                .iter()
                .map(|t| (t.key.to_vec(), t.raw_value.to_vec()))
                .collect(),
            source: msg.source().map(|s| s.raw.to_vec()),
            command: msg.command().to_vec(),
            params: msg.params().iter().map(|p| p.to_vec()).collect(),
            // Preserve the wire form so re-serialising reproduces the bytes.
            force_trailing: msg.has_trailing_param(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MessageBuf;
    use crate::{Message, numeric};

    #[test]
    fn builds_a_numeric_reply() {
        let msg = MessageBuf::numeric(numeric::RPL_WELCOME)
            .source("irc.example.org")
            .param("nick")
            .trailing("Welcome");
        assert_eq!(
            msg.to_vec().unwrap(),
            b":irc.example.org 001 nick :Welcome\r\n"
        );
    }

    #[test]
    fn escapes_tag_values_automatically() {
        let msg = MessageBuf::new("TAGMSG")
            .tag("+kestrel.chat/rtc", "a;b c")
            .param("#chan");
        assert_eq!(
            msg.to_vec().unwrap(),
            b"@+kestrel.chat/rtc=a\\:b\\sc TAGMSG #chan\r\n"
        );
    }

    #[test]
    fn accepts_a_batch_of_params() {
        let msg = MessageBuf::new("MODE").params(["#chan", "+nt"]);
        assert_eq!(msg.to_vec().unwrap(), b"MODE #chan +nt\r\n");
    }

    #[test]
    fn round_trips_through_a_borrowed_message() {
        let lines: &[&[u8]] = &[
            b"PING token\r\n",
            b"PRIVMSG #chan :hello world\r\n",
            b"PRIVMSG #chan hi\r\n",
            b"@a=1;b=2 :srv 001 nick :Welcome\r\n",
            b"@x=a\\:b :nick!u@h TAGMSG #chan\r\n",
        ];
        for line in lines {
            let parsed = Message::parse(line).expect("should parse");
            let owned = MessageBuf::from(&parsed);
            assert_eq!(owned.to_vec().unwrap(), *line, "changed {line:?}");
        }
    }
}
