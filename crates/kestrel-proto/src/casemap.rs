//! Case-insensitive comparison of nicknames and channel names.
//!
//! IRC compares names case-insensitively, but *which* bytes count as the same
//! letter depends on the network: the server announces its choice in the
//! `CASEMAPPING` token of `RPL_ISUPPORT`. Getting this wrong is not cosmetic.
//! On an `rfc1459` network `nick[tab]` and `nick{tab}` are the *same user*, so
//! a client or server that compares with plain ASCII rules will hand one
//! person's private messages to another.

/// How a network folds case when comparing names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseMapping {
    /// Only `A`–`Z` fold to `a`–`z`.
    Ascii,
    /// ASCII folding plus `[`, `]`, `\` and `~`, which fold to `{`, `}`, `|`
    /// and `^`. This is the historical default and what most networks use, on
    /// the theory that those characters are the Scandinavian letters.
    #[default]
    Rfc1459,
    /// As [`CaseMapping::Rfc1459`] but without the `~`/`^` pairing.
    Rfc1459Strict,
}

impl CaseMapping {
    /// Parse the value of the `CASEMAPPING` `RPL_ISUPPORT` token.
    ///
    /// An unrecognised value falls back to [`CaseMapping::Rfc1459`], which is
    /// the safer guess: it folds together strictly more characters than ASCII,
    /// so two names we treat as equal are never treated as distinct by the
    /// server. Guessing the other way would risk a nick collision we failed to
    /// notice.
    #[must_use]
    pub fn parse(value: &[u8]) -> Self {
        if value.eq_ignore_ascii_case(b"ascii") {
            Self::Ascii
        } else if value.eq_ignore_ascii_case(b"rfc1459-strict") {
            Self::Rfc1459Strict
        } else {
            Self::Rfc1459
        }
    }

    /// Fold a single byte to its lowercase form under this mapping.
    #[must_use]
    pub fn fold_byte(self, byte: u8) -> u8 {
        match self {
            Self::Ascii => byte.to_ascii_lowercase(),
            Self::Rfc1459 => match byte {
                b'[' => b'{',
                b']' => b'}',
                b'\\' => b'|',
                b'~' => b'^',
                other => other.to_ascii_lowercase(),
            },
            Self::Rfc1459Strict => match byte {
                b'[' => b'{',
                b']' => b'}',
                b'\\' => b'|',
                other => other.to_ascii_lowercase(),
            },
        }
    }

    /// Whether two names are the same under this mapping.
    #[must_use]
    pub fn eq(self, a: &[u8], b: &[u8]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(&x, &y)| self.fold_byte(x) == self.fold_byte(y))
    }

    /// Fold a whole name, for use as a lookup key.
    #[must_use]
    pub fn fold(self, name: &[u8]) -> Vec<u8> {
        name.iter().map(|&b| self.fold_byte(b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::CaseMapping::{Ascii, Rfc1459, Rfc1459Strict};

    #[test]
    fn ascii_folds_only_letters() {
        assert!(Ascii.eq(b"Nick", b"nick"));
        assert!(!Ascii.eq(b"nick[", b"nick{"));
    }

    #[test]
    fn rfc1459_folds_the_scandinavian_pairs() {
        assert!(Rfc1459.eq(b"nick[]\\~", b"nick{}|^"));
        assert!(Rfc1459.eq(b"A[B]C", b"a{b}c"));
    }

    #[test]
    fn strict_variant_leaves_tilde_alone() {
        assert!(Rfc1459Strict.eq(b"nick[]\\", b"nick{}|"));
        assert!(!Rfc1459Strict.eq(b"nick~", b"nick^"));
    }

    #[test]
    fn folding_agrees_with_comparison() {
        for (a, b) in [
            (&b"Nick"[..], &b"nIcK"[..]),
            (b"chan[1]", b"CHAN{1}"),
            (b"", b""),
        ] {
            assert_eq!(Rfc1459.eq(a, b), Rfc1459.fold(a) == Rfc1459.fold(b));
        }
    }

    #[test]
    fn different_lengths_are_never_equal() {
        assert!(!Rfc1459.eq(b"nick", b"nick2"));
    }

    #[test]
    fn unknown_casemapping_token_falls_back_to_rfc1459() {
        assert_eq!(super::CaseMapping::parse(b"ascii"), Ascii);
        assert_eq!(super::CaseMapping::parse(b"ASCII"), Ascii);
        assert_eq!(super::CaseMapping::parse(b"rfc1459-strict"), Rfc1459Strict);
        assert_eq!(super::CaseMapping::parse(b"rfc1459"), Rfc1459);
        assert_eq!(super::CaseMapping::parse(b"rfc7613"), Rfc1459);
        assert_eq!(super::CaseMapping::parse(b""), Rfc1459);
    }
}
