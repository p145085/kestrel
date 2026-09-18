//! `RPL_ISUPPORT`: what this particular network allows.
//!
//! Every network differs in nickname length, channel prefixes, mode letters
//! and — most consequentially — how it folds case. A client that assumes
//! defaults will mis-compare names on some networks and silently deliver a
//! message to the wrong window, so these are read rather than guessed.

use std::collections::HashMap;

use kestrel_proto::{CaseMapping, Message};

/// A channel membership prefix and the mode letter that grants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    /// The mode letter, such as `o`.
    pub mode: u8,
    /// The character shown before a nickname, such as `@`.
    pub symbol: u8,
}

/// What a server said about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ISupport {
    tokens: HashMap<String, Option<String>>,
    casemapping: CaseMapping,
    chantypes: Vec<u8>,
    prefixes: Vec<Prefix>,
    /// Mode letters by `CHANMODES` type: list, always-param, param-on-set, none.
    chanmodes: [Vec<u8>; 4],
    network: Option<String>,
    nicklen: usize,
    channellen: usize,
    topiclen: usize,
}

impl Default for ISupport {
    fn default() -> Self {
        Self {
            tokens: HashMap::new(),
            // RFC 1459 folding is the safer default: it treats strictly more
            // names as equal than ASCII does, so two names we consider the
            // same are never treated as distinct by the server. Guessing the
            // other way risks a collision we failed to notice.
            casemapping: CaseMapping::Rfc1459,
            chantypes: b"#&".to_vec(),
            prefixes: vec![
                Prefix {
                    mode: b'o',
                    symbol: b'@',
                },
                Prefix {
                    mode: b'v',
                    symbol: b'+',
                },
            ],
            chanmodes: [
                b"b".to_vec(),
                b"k".to_vec(),
                b"l".to_vec(),
                b"imnst".to_vec(),
            ],
            network: None,
            nicklen: 9,
            channellen: 50,
            topiclen: 390,
        }
    }
}

impl ISupport {
    /// An empty set holding the conservative defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Absorb one `RPL_ISUPPORT` message.
    ///
    /// The first parameter is the client's nickname and the last is human
    /// text; everything between is a token.
    pub fn absorb(&mut self, msg: &Message<'_>) {
        let params = msg.params();
        if params.len() < 2 {
            return;
        }
        for token in &params[1..params.len().saturating_sub(1)] {
            self.absorb_token(token);
        }
    }

    fn absorb_token(&mut self, token: &[u8]) {
        // A leading `-` removes a token the server previously advertised.
        if let Some(removed) = token.strip_prefix(b"-") {
            let name = String::from_utf8_lossy(removed).to_uppercase();
            self.tokens.remove(&name);
            return;
        }

        let (name, value) = match token.iter().position(|&b| b == b'=') {
            Some(i) => (&token[..i], Some(&token[i + 1..])),
            None => (token, None),
        };
        let name = String::from_utf8_lossy(name).to_uppercase();
        let text = value.map(|v| String::from_utf8_lossy(v).into_owned());

        match name.as_str() {
            "CASEMAPPING" => {
                if let Some(value) = value {
                    self.casemapping = CaseMapping::parse(value);
                }
            }
            "CHANTYPES" => {
                if let Some(value) = value {
                    self.chantypes = value.to_vec();
                }
            }
            "PREFIX" => {
                if let Some(value) = value {
                    self.set_prefixes(value);
                }
            }
            "CHANMODES" => {
                if let Some(value) = value {
                    self.set_chanmodes(value);
                }
            }
            "NETWORK" => self.network.clone_from(&text),
            "NICKLEN" => self.nicklen = parse_usize(value).unwrap_or(self.nicklen),
            "CHANNELLEN" => self.channellen = parse_usize(value).unwrap_or(self.channellen),
            "TOPICLEN" => self.topiclen = parse_usize(value).unwrap_or(self.topiclen),
            _ => {}
        }
        self.tokens.insert(name, text);
    }

    /// `PREFIX=(ov)@+` — mode letters in parentheses, then their symbols.
    fn set_prefixes(&mut self, value: &[u8]) {
        let Some(close) = value.iter().position(|&b| b == b')') else {
            return;
        };
        if value.first() != Some(&b'(') {
            return;
        }
        let modes = &value[1..close];
        let symbols = &value[close + 1..];
        if modes.len() != symbols.len() {
            // Mismatched halves would pair the wrong symbol with the wrong
            // mode; keeping the defaults is less wrong than guessing.
            return;
        }
        self.prefixes = modes
            .iter()
            .zip(symbols.iter())
            .map(|(&mode, &symbol)| Prefix { mode, symbol })
            .collect();
    }

    /// `CHANMODES=b,k,l,imnst` — four comma-separated groups.
    fn set_chanmodes(&mut self, value: &[u8]) {
        let groups: Vec<&[u8]> = value.split(|&b| b == b',').collect();
        for (index, group) in groups.iter().take(4).enumerate() {
            self.chanmodes[index] = (*group).to_vec();
        }
    }

    /// How this network folds case when comparing names.
    #[must_use]
    pub fn casemapping(&self) -> CaseMapping {
        self.casemapping
    }

    /// Whether `name` looks like a channel on this network.
    #[must_use]
    pub fn is_channel(&self, name: &[u8]) -> bool {
        name.first().is_some_and(|b| self.chantypes.contains(b))
    }

    /// Characters that may start a channel name.
    #[must_use]
    pub fn chantypes(&self) -> &[u8] {
        &self.chantypes
    }

    /// Membership prefixes, highest-ranking first.
    #[must_use]
    pub fn prefixes(&self) -> &[Prefix] {
        &self.prefixes
    }

    /// Every prefix symbol, highest-ranking first.
    #[must_use]
    pub fn prefix_symbols(&self) -> Vec<u8> {
        self.prefixes.iter().map(|p| p.symbol).collect()
    }

    /// Strip and return the membership prefixes from the front of a nickname.
    ///
    /// `NAMES` returns entries like `@alice`, or `@+alice` on a network where
    /// `multi-prefix` is in play.
    #[must_use]
    pub fn split_prefixes<'a>(&self, entry: &'a [u8]) -> (Vec<u8>, &'a [u8]) {
        let symbols = self.prefix_symbols();
        let count = entry.iter().take_while(|b| symbols.contains(b)).count();
        (entry[..count].to_vec(), &entry[count..])
    }

    /// Mode letters of the given `CHANMODES` type, 0 through 3.
    #[must_use]
    pub fn chanmodes(&self, kind: usize) -> &[u8] {
        self.chanmodes.get(kind).map_or(&[], Vec::as_slice)
    }

    /// The advertised network name.
    #[must_use]
    pub fn network(&self) -> Option<&str> {
        self.network.as_deref()
    }

    /// Longest permitted nickname.
    #[must_use]
    pub fn nicklen(&self) -> usize {
        self.nicklen
    }

    /// Longest permitted channel name.
    #[must_use]
    pub fn channellen(&self) -> usize {
        self.channellen
    }

    /// Longest permitted topic.
    #[must_use]
    pub fn topiclen(&self) -> usize {
        self.topiclen
    }

    /// The raw value of a token, if the server advertised it.
    ///
    /// `Some(None)` means the token was advertised without a value.
    #[must_use]
    pub fn token(&self, name: &str) -> Option<Option<&str>> {
        self.tokens
            .get(&name.to_uppercase())
            .map(std::option::Option::as_deref)
    }

    /// Whether the server advertised a token at all.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.tokens.contains_key(&name.to_uppercase())
    }

    /// Compare two names using this network's casemapping.
    #[must_use]
    pub fn eq(&self, a: &[u8], b: &[u8]) -> bool {
        self.casemapping.eq(a, b)
    }

    /// Fold a name into a lookup key for this network.
    #[must_use]
    pub fn fold(&self, name: &[u8]) -> Vec<u8> {
        self.casemapping.fold(name)
    }
}

fn parse_usize(value: Option<&[u8]>) -> Option<usize> {
    std::str::from_utf8(value?).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{ISupport, Prefix};
    use kestrel_proto::{CaseMapping, Message};

    fn absorb(support: &mut ISupport, line: &str) {
        let bytes = line.as_bytes().to_vec();
        let msg = Message::parse(&bytes).expect("should parse");
        support.absorb(&msg);
    }

    #[test]
    fn defaults_are_conservative_before_anything_is_said() {
        let support = ISupport::new();
        assert_eq!(support.casemapping(), CaseMapping::Rfc1459);
        assert!(support.is_channel(b"#chan"));
        assert!(!support.is_channel(b"chan"));
        assert_eq!(support.prefix_symbols(), b"@+");
    }

    #[test]
    fn tokens_are_absorbed_from_a_real_reply() {
        let mut support = ISupport::new();
        absorb(
            &mut support,
            ":srv 005 alice NETWORK=Kestrel NICKLEN=32 CHANNELLEN=64 TOPICLEN=390 \
             CASEMAPPING=ascii CHANTYPES=#&! :are supported by this server",
        );

        assert_eq!(support.network(), Some("Kestrel"));
        assert_eq!(support.nicklen(), 32);
        assert_eq!(support.channellen(), 64);
        assert_eq!(support.topiclen(), 390);
        assert_eq!(support.casemapping(), CaseMapping::Ascii);
        assert!(support.is_channel(b"!local"));
    }

    #[test]
    fn the_trailing_text_is_not_mistaken_for_a_token() {
        let mut support = ISupport::new();
        absorb(
            &mut support,
            ":srv 005 alice NETWORK=Kestrel :are supported by this server",
        );
        assert!(!support.has("are"));
        assert!(support.has("NETWORK"));
    }

    #[test]
    fn prefixes_pair_modes_with_their_symbols() {
        let mut support = ISupport::new();
        absorb(&mut support, ":srv 005 alice PREFIX=(qaohv)~&@%+ :x");
        assert_eq!(
            support.prefixes(),
            [
                Prefix {
                    mode: b'q',
                    symbol: b'~'
                },
                Prefix {
                    mode: b'a',
                    symbol: b'&'
                },
                Prefix {
                    mode: b'o',
                    symbol: b'@'
                },
                Prefix {
                    mode: b'h',
                    symbol: b'%'
                },
                Prefix {
                    mode: b'v',
                    symbol: b'+'
                },
            ]
        );
    }

    #[test]
    fn a_malformed_prefix_token_keeps_the_defaults() {
        // Pairing the wrong symbol with the wrong mode would show the wrong
        // rank against every nickname in the channel.
        let mut support = ISupport::new();
        absorb(&mut support, ":srv 005 alice PREFIX=(ovx)@+ :x");
        assert_eq!(support.prefix_symbols(), b"@+");
    }

    #[test]
    fn prefixes_are_stripped_from_names_entries() {
        let mut support = ISupport::new();
        absorb(&mut support, ":srv 005 alice PREFIX=(ov)@+ :x");

        assert_eq!(
            support.split_prefixes(b"@alice"),
            (b"@".to_vec(), &b"alice"[..])
        );
        assert_eq!(
            support.split_prefixes(b"@+alice"),
            (b"@+".to_vec(), &b"alice"[..])
        );
        assert_eq!(
            support.split_prefixes(b"alice"),
            (Vec::new(), &b"alice"[..])
        );
    }

    #[test]
    fn chanmodes_are_split_into_their_four_kinds() {
        let mut support = ISupport::new();
        absorb(&mut support, ":srv 005 alice CHANMODES=beI,k,l,imnpst :x");
        assert_eq!(support.chanmodes(0), b"beI");
        assert_eq!(support.chanmodes(1), b"k");
        assert_eq!(support.chanmodes(2), b"l");
        assert_eq!(support.chanmodes(3), b"imnpst");
    }

    #[test]
    fn a_valueless_token_is_still_recorded() {
        let mut support = ISupport::new();
        absorb(&mut support, ":srv 005 alice SAFELIST WHOX :x");
        assert!(support.has("SAFELIST"));
        assert_eq!(support.token("SAFELIST"), Some(None));
        assert!(support.has("whox"), "token lookup should ignore case");
    }

    #[test]
    fn a_negated_token_removes_a_previous_one() {
        let mut support = ISupport::new();
        absorb(&mut support, ":srv 005 alice SAFELIST :x");
        assert!(support.has("SAFELIST"));
        absorb(&mut support, ":srv 005 alice -SAFELIST :x");
        assert!(!support.has("SAFELIST"));
    }

    #[test]
    fn casemapping_drives_comparison() {
        let mut support = ISupport::new();
        absorb(&mut support, ":srv 005 alice CASEMAPPING=rfc1459 :x");
        assert!(support.eq(b"nick[x]", b"nick{x}"));

        absorb(&mut support, ":srv 005 alice CASEMAPPING=ascii :x");
        assert!(!support.eq(b"nick[x]", b"nick{x}"));
        assert!(support.eq(b"Nick", b"nick"));
    }

    #[test]
    fn a_nonsense_length_is_ignored_rather_than_zeroing_the_limit() {
        let mut support = ISupport::new();
        absorb(&mut support, ":srv 005 alice NICKLEN=lots :x");
        assert_eq!(support.nicklen(), 9, "the previous value should survive");
    }
}
