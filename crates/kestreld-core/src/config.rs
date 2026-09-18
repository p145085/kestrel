//! Server configuration.

use kestrel_proto::CaseMapping;

/// Static configuration for a running server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The server's own name, used as the source of messages it originates.
    pub server_name: Vec<u8>,
    /// Human-readable network name, advertised as the `NETWORK` token.
    pub network_name: Vec<u8>,
    /// Version string reported in `RPL_YOURHOST` and `RPL_MYINFO`.
    pub version: Vec<u8>,
    /// Message of the day, one entry per line. Empty means none is configured.
    pub motd: Vec<Vec<u8>>,
    /// How names are compared. Changing this on a live network is not safe:
    /// two nicks that were distinct may become the same.
    pub casemapping: CaseMapping,
    /// Longest permitted nickname.
    pub max_nick_len: usize,
    /// Longest permitted channel name.
    pub max_channel_len: usize,
    /// Longest permitted topic.
    pub max_topic_len: usize,
    /// Longest permitted quit or part reason.
    pub max_reason_len: usize,
    /// How many channels one client may be in at once.
    pub max_channels_per_client: usize,
    /// Characters that may start a channel name.
    pub channel_prefixes: Vec<u8>,
    /// A password every client must supply via `PASS`, if any.
    pub password: Option<Vec<u8>>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server_name: b"kestrel.local".to_vec(),
            network_name: b"Kestrel".to_vec(),
            version: format!("kestreld-{}", env!("CARGO_PKG_VERSION")).into_bytes(),
            motd: Vec::new(),
            casemapping: CaseMapping::Rfc1459,
            max_nick_len: 32,
            max_channel_len: 64,
            max_topic_len: 390,
            max_reason_len: 300,
            max_channels_per_client: 128,
            channel_prefixes: b"#&".to_vec(),
            password: None,
        }
    }
}

impl ServerConfig {
    /// Whether `name` is a syntactically valid channel name.
    #[must_use]
    pub fn is_valid_channel(&self, name: &[u8]) -> bool {
        !name.is_empty()
            && name.len() <= self.max_channel_len
            && self.channel_prefixes.contains(&name[0])
            // A channel name may not contain a space, comma, or bell: space
            // and comma are protocol separators, and bell is historically
            // disallowed.
            && !name.iter().any(|&b| matches!(b, b' ' | b',' | 0x07))
    }

    /// Whether `nick` is a syntactically valid nickname.
    ///
    /// The permitted set is deliberately conservative. A nickname that can be
    /// confused with a channel name, a mode prefix, or a message separator is
    /// a source of spoofing, so anything ambiguous is rejected rather than
    /// escaped somewhere downstream.
    #[must_use]
    pub fn is_valid_nick(&self, nick: &[u8]) -> bool {
        if nick.is_empty() || nick.len() > self.max_nick_len {
            return false;
        }
        // A leading digit or '-' would make the nick ambiguous with a numeric
        // or an option, so the first character is restricted further.
        let first_ok = nick[0].is_ascii_alphabetic() || SPECIAL_NICK_BYTES.contains(&nick[0]);
        first_ok
            && nick[1..]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || SPECIAL_NICK_BYTES.contains(b) || *b == b'-')
    }
}

/// Non-alphanumeric bytes permitted in a nickname, per RFC 2812.
const SPECIAL_NICK_BYTES: &[u8] = b"[]\\`_^{|}";

#[cfg(test)]
mod tests {
    use super::ServerConfig;

    #[test]
    fn accepts_ordinary_nicks() {
        let cfg = ServerConfig::default();
        for nick in [&b"alice"[..], b"Bob", b"nick_", b"a", b"x[1]", b"we-ird"] {
            assert!(cfg.is_valid_nick(nick), "should accept {nick:?}");
        }
    }

    #[test]
    fn rejects_ambiguous_or_malformed_nicks() {
        let cfg = ServerConfig::default();
        for nick in [
            &b""[..],
            b"1alice",   // could be confused with a numeric
            b"-alice",   // could be confused with an option
            b"#channel", // would be confused with a channel
            b"@op",      // would be confused with a mode prefix
            b"has space",
            b"comma,nick",
            b":colon",
        ] {
            assert!(!cfg.is_valid_nick(nick), "should reject {nick:?}");
        }
    }

    #[test]
    fn enforces_the_nick_length_limit() {
        let cfg = ServerConfig {
            max_nick_len: 4,
            ..ServerConfig::default()
        };
        assert!(cfg.is_valid_nick(b"abcd"));
        assert!(!cfg.is_valid_nick(b"abcde"));
    }

    #[test]
    fn accepts_channels_with_a_configured_prefix() {
        let cfg = ServerConfig::default();
        assert!(cfg.is_valid_channel(b"#chan"));
        assert!(cfg.is_valid_channel(b"&local"));
        assert!(!cfg.is_valid_channel(b"chan"));
        assert!(!cfg.is_valid_channel(b""));
    }

    #[test]
    fn rejects_channels_containing_protocol_separators() {
        let cfg = ServerConfig::default();
        assert!(!cfg.is_valid_channel(b"#has space"));
        assert!(!cfg.is_valid_channel(b"#has,comma"));
        assert!(!cfg.is_valid_channel(b"#has\x07bell"));
    }
}
