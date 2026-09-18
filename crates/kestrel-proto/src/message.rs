//! Parsing and serialising IRC protocol messages.

use smallvec::SmallVec;

use crate::error::{ParseError, SerializeError};
use crate::source::Source;
use crate::tags::{self, Tag, TagVec};

/// Parameters of a message, stored as borrowed slices.
pub type ParamVec<'a> = SmallVec<[&'a [u8]; 8]>;

/// A parsed IRC message, borrowing from the buffer it was parsed out of.
///
/// Byte slices rather than `str` throughout: IRC has no guaranteed encoding,
/// and a client that panics or silently drops a message on invalid UTF-8 is a
/// broken client. Use the `_str` accessors where lossy text is wanted.
#[derive(Debug, Clone)]
pub struct Message<'a> {
    tags: TagVec<'a>,
    source: Option<Source<'a>>,
    command: &'a [u8],
    params: ParamVec<'a>,
    /// Write the final parameter in trailing form even when it would not
    /// strictly need it. Presentation only: `PRIVMSG #c hi` and
    /// `PRIVMSG #c :hi` carry identical content, so this is excluded from
    /// equality.
    force_trailing: bool,
}

impl PartialEq for Message<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.tags == other.tags
            && self.source == other.source
            && self.command == other.command
            && self.params == other.params
    }
}

impl Eq for Message<'_> {}

impl<'a> Message<'a> {
    /// Start building a message with the given command.
    #[must_use]
    pub fn new(command: &'a [u8]) -> Self {
        Self {
            tags: TagVec::new(),
            source: None,
            command,
            params: ParamVec::new(),
            force_trailing: false,
        }
    }

    /// Attach a tag whose value is already in escaped wire form.
    #[must_use]
    pub fn with_raw_tag(mut self, key: &'a [u8], raw_value: &'a [u8]) -> Self {
        self.tags.push(Tag { key, raw_value });
        self
    }

    /// Set the source.
    #[must_use]
    pub fn with_source(mut self, source: Source<'a>) -> Self {
        self.source = Some(source);
        self
    }

    /// Append a parameter.
    #[must_use]
    pub fn with_param(mut self, param: &'a [u8]) -> Self {
        self.params.push(param);
        self
    }

    /// Append a final parameter and write it in trailing form, prefixed with
    /// `:`, even when it would not strictly need it.
    ///
    /// Use this for free-text parameters such as the message body of
    /// `PRIVMSG`, a `QUIT` reason, or a topic: the text is user-supplied, so
    /// its shape is not known in advance, and the trailing form is what every
    /// implementation expects to see there.
    #[must_use]
    pub fn with_trailing_param(mut self, param: &'a [u8]) -> Self {
        self.params.push(param);
        self.force_trailing = true;
        self
    }

    /// The message tags.
    #[must_use]
    pub fn tags(&self) -> &[Tag<'a>] {
        &self.tags
    }

    /// Look up a tag by its exact key, including any `+` and vendor prefix.
    #[must_use]
    pub fn tag(&self, key: &[u8]) -> Option<&Tag<'a>> {
        self.tags.iter().find(|t| t.key == key)
    }

    /// The message source, if it carried one.
    #[must_use]
    pub fn source(&self) -> Option<&Source<'a>> {
        self.source.as_ref()
    }

    /// The command verb or numeric, as it appeared on the wire.
    #[must_use]
    pub fn command(&self) -> &'a [u8] {
        self.command
    }

    /// The command as lossy UTF-8, for logging and display.
    #[must_use]
    pub fn command_str(&self) -> std::borrow::Cow<'a, str> {
        String::from_utf8_lossy(self.command)
    }

    /// Whether the command matches `other`, compared ASCII-case-insensitively
    /// as the protocol requires.
    #[must_use]
    pub fn is_command(&self, other: &[u8]) -> bool {
        self.command.eq_ignore_ascii_case(other)
    }

    /// The command as a three-digit numeric reply code, if it is one.
    #[must_use]
    pub fn numeric(&self) -> Option<u16> {
        if self.command.len() != 3 || !self.command.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let digit = |i: usize| u16::from(self.command[i] - b'0');
        Some(digit(0) * 100 + digit(1) * 10 + digit(2))
    }

    /// The message parameters.
    #[must_use]
    pub fn params(&self) -> &[&'a [u8]] {
        &self.params
    }

    /// The parameter at `index`, if present.
    #[must_use]
    pub fn param(&self, index: usize) -> Option<&'a [u8]> {
        self.params.get(index).copied()
    }

    /// Whether the final parameter is written in trailing form.
    ///
    /// This is presentation, not content: `PRIVMSG #c hi` and
    /// `PRIVMSG #c :hi` carry the same parameters and compare equal. It is
    /// exposed so that code copying a message can reproduce the original bytes.
    #[must_use]
    pub fn has_trailing_param(&self) -> bool {
        self.force_trailing
    }

    /// Parse one line into a message.
    ///
    /// Trailing CR and LF bytes are stripped, so a line may be passed either
    /// with or without its terminator.
    pub fn parse(input: &'a [u8]) -> Result<Self, ParseError> {
        let mut rest = strip_terminator(input);
        if rest.is_empty() {
            return Err(ParseError::Empty);
        }

        let mut tags = TagVec::new();
        if rest.first() == Some(&b'@') {
            let after_at = &rest[1..];
            let end = memchr::memchr(b' ', after_at).ok_or(ParseError::MissingCommand)?;
            tags::parse_section(&after_at[..end], &mut tags);
            rest = skip_spaces(&after_at[end..]);
        }

        let mut source = None;
        if rest.first() == Some(&b':') {
            let after_colon = &rest[1..];
            let end = memchr::memchr(b' ', after_colon).unwrap_or(after_colon.len());
            source = Some(Source::parse(&after_colon[..end]));
            rest = skip_spaces(&after_colon[end..]);
        }

        let end = memchr::memchr(b' ', rest).unwrap_or(rest.len());
        let command = &rest[..end];
        if command.is_empty() {
            return Err(ParseError::MissingCommand);
        }
        rest = &rest[end..];

        let mut params = ParamVec::new();
        let mut force_trailing = false;
        loop {
            rest = skip_spaces(rest);
            if rest.is_empty() {
                break;
            }
            if rest[0] == b':' {
                params.push(&rest[1..]);
                // Remember the trailing form so that re-serialising a parsed
                // message reproduces the original bytes.
                force_trailing = true;
                break;
            }
            let end = memchr::memchr(b' ', rest).unwrap_or(rest.len());
            params.push(&rest[..end]);
            rest = &rest[end..];
        }

        Ok(Self {
            tags,
            source,
            command,
            params,
            force_trailing,
        })
    }

    /// Write the message to `out`, terminated by CRLF.
    ///
    /// Tag values are written exactly as held, so a message built with
    /// [`Message::with_raw_tag`] must be given values that are already escaped.
    pub fn write_to(&self, out: &mut Vec<u8>) -> Result<(), SerializeError> {
        if self.command.is_empty() {
            return Err(SerializeError::EmptyCommand);
        }
        self.validate_params()?;

        if !self.tags.is_empty() {
            out.push(b'@');
            for (i, tag) in self.tags.iter().enumerate() {
                if i > 0 {
                    out.push(b';');
                }
                out.extend_from_slice(tag.key);
                if !tag.raw_value.is_empty() {
                    out.push(b'=');
                    out.extend_from_slice(tag.raw_value);
                }
            }
            out.push(b' ');
        }

        if let Some(source) = &self.source {
            out.push(b':');
            out.extend_from_slice(source.raw);
            out.push(b' ');
        }

        out.extend_from_slice(self.command);

        let last = self.params.len().saturating_sub(1);
        for (i, param) in self.params.iter().enumerate() {
            out.push(b' ');
            if i == last && (self.force_trailing || needs_trailing_form(param)) {
                out.push(b':');
            }
            out.extend_from_slice(param);
        }

        out.extend_from_slice(b"\r\n");
        Ok(())
    }

    /// Serialise to a fresh buffer.
    pub fn to_vec(&self) -> Result<Vec<u8>, SerializeError> {
        let mut out = Vec::with_capacity(64);
        self.write_to(&mut out)?;
        Ok(out)
    }

    fn validate_params(&self) -> Result<(), SerializeError> {
        let last = self.params.len().saturating_sub(1);
        for (i, param) in self.params.iter().enumerate() {
            if param.iter().any(|&b| matches!(b, b'\r' | b'\n' | 0)) {
                return Err(SerializeError::ParamHasControlByte { index: i });
            }
            if i != last && needs_trailing_form(param) {
                return Err(SerializeError::ParamMustBeLast { index: i });
            }
        }
        Ok(())
    }
}

/// Whether a parameter can only be represented as the trailing parameter.
fn needs_trailing_form(param: &[u8]) -> bool {
    param.is_empty() || param.first() == Some(&b':') || memchr::memchr(b' ', param).is_some()
}

fn strip_terminator(mut input: &[u8]) -> &[u8] {
    while matches!(input.last(), Some(b'\r' | b'\n')) {
        input = &input[..input.len() - 1];
    }
    input
}

fn skip_spaces(input: &[u8]) -> &[u8] {
    let n = input.iter().take_while(|&&b| b == b' ').count();
    &input[n..]
}
