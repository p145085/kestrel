//! IRCv3 message tags: parsing, escaping, and client-only tag handling.

use std::borrow::Cow;

use smallvec::SmallVec;

/// Tags attached to a message, stored as borrowed key/value slices.
///
/// Values are kept in their escaped wire form; [`Tag::value`] unescapes, and
/// allocates only when the value actually contains an escape sequence.
pub type TagVec<'a> = SmallVec<[Tag<'a>; 8]>;

/// A single message tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag<'a> {
    /// The tag key, including any `+` client-only prefix and vendor prefix.
    pub key: &'a [u8],
    /// The tag value exactly as it appeared on the wire, still escaped.
    pub raw_value: &'a [u8],
}

impl<'a> Tag<'a> {
    /// The unescaped value.
    ///
    /// A tag written without `=` has an empty value, which the specification
    /// says to treat identically to one written as `key=`.
    #[must_use]
    pub fn value(&self) -> Cow<'a, [u8]> {
        unescape(self.raw_value)
    }

    /// Whether this is a client-only tag — one the server relays verbatim
    /// without interpreting it.
    #[must_use]
    pub fn is_client_only(&self) -> bool {
        self.key.first() == Some(&b'+')
    }

    /// The key with any `+` prefix removed.
    #[must_use]
    pub fn unprefixed_key(&self) -> &'a [u8] {
        if self.is_client_only() {
            &self.key[1..]
        } else {
            self.key
        }
    }

    /// The vendor part of the key, if the key is vendor-namespaced.
    ///
    /// For `+kestrel.chat/rtc` this is `kestrel.chat`.
    #[must_use]
    pub fn vendor(&self) -> Option<&'a [u8]> {
        let key = self.unprefixed_key();
        memchr::memchr(b'/', key).map(|i| &key[..i])
    }

    /// The key with the `+` prefix and any vendor namespace removed.
    #[must_use]
    pub fn name(&self) -> &'a [u8] {
        let key = self.unprefixed_key();
        match memchr::memchr(b'/', key) {
            Some(i) => &key[i + 1..],
            None => key,
        }
    }
}

/// Parse a tag section — the text between the leading `@` and the separating
/// space — appending each tag to `out`.
///
/// Parsing is deliberately permissive: empty entries and entries with an empty
/// key are skipped rather than rejected, because one malformed tag from a peer
/// should not cost us an otherwise valid message.
pub fn parse_section<'a>(section: &'a [u8], out: &mut TagVec<'a>) {
    for raw in section.split(|&b| b == b';') {
        if raw.is_empty() {
            continue;
        }
        let (key, raw_value) = match memchr::memchr(b'=', raw) {
            Some(i) => (&raw[..i], &raw[i + 1..]),
            None => (raw, &raw[raw.len()..]),
        };
        if key.is_empty() {
            continue;
        }
        out.push(Tag { key, raw_value });
    }
}

/// Unescape a tag value.
///
/// The escape table is:
///
/// | Sequence | Becomes |
/// |---|---|
/// | `\:` | `;` |
/// | `\s` | space |
/// | `\\` | `\` |
/// | `\r` | CR |
/// | `\n` | LF |
///
/// A backslash before any other character is dropped and that character kept
/// literally. A lone trailing backslash is dropped entirely.
#[must_use]
pub fn unescape(raw: &[u8]) -> Cow<'_, [u8]> {
    if memchr::memchr(b'\\', raw).is_none() {
        return Cow::Borrowed(raw);
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != b'\\' {
            out.push(raw[i]);
            i += 1;
            continue;
        }
        match raw.get(i + 1) {
            Some(b':') => out.push(b';'),
            Some(b's') => out.push(b' '),
            Some(b'\\') => out.push(b'\\'),
            Some(b'r') => out.push(b'\r'),
            Some(b'n') => out.push(b'\n'),
            Some(&other) => out.push(other),
            // Lone trailing backslash: dropped.
            None => break,
        }
        i += 2;
    }
    Cow::Owned(out)
}

/// Escape a tag value for the wire, the inverse of [`unescape`].
#[must_use]
pub fn escape(value: &[u8]) -> Cow<'_, [u8]> {
    let needs_escaping = value
        .iter()
        .any(|&b| matches!(b, b';' | b' ' | b'\\' | b'\r' | b'\n'));
    if !needs_escaping {
        return Cow::Borrowed(value);
    }
    let mut out = Vec::with_capacity(value.len() + 8);
    for &b in value {
        match b {
            b';' => out.extend_from_slice(b"\\:"),
            b' ' => out.extend_from_slice(b"\\s"),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\n' => out.extend_from_slice(b"\\n"),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}
