//! Message sources (the `:nick!user@host` prefix).

/// The origin of a message.
///
/// A source is either a server name or a user mask. The two are distinguished
/// by the presence of `!` or `@`: a bare word is treated as a server name,
/// though in practice a bare nick is also legal and indistinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source<'a> {
    /// The source exactly as it appeared, without the leading `:`.
    pub raw: &'a [u8],
    /// Nickname, or server name when [`Source::is_server`] is true.
    pub nick: &'a [u8],
    /// Username / ident, when present.
    pub user: Option<&'a [u8]>,
    /// Hostname, when present.
    pub host: Option<&'a [u8]>,
}

impl<'a> Source<'a> {
    /// Split a raw source into its components.
    #[must_use]
    pub fn parse(raw: &'a [u8]) -> Self {
        let (name_part, host) = match memchr::memchr(b'@', raw) {
            Some(i) => (&raw[..i], Some(&raw[i + 1..])),
            None => (raw, None),
        };
        let (nick, user) = match memchr::memchr(b'!', name_part) {
            Some(i) => (&name_part[..i], Some(&name_part[i + 1..])),
            None => (name_part, None),
        };
        Self {
            raw,
            nick,
            user,
            host,
        }
    }

    /// Whether this looks like a server name rather than a user.
    ///
    /// Servers never carry a user or host component.
    #[must_use]
    pub fn is_server(&self) -> bool {
        self.user.is_none() && self.host.is_none()
    }
}
