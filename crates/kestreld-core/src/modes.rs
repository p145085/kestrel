//! Parsing mode change strings.
//!
//! A `MODE` line looks like `MODE #chan +ovk-l alice bob secret`, where which
//! letters consume a parameter depends on the letter, and on whether it is
//! being set or unset. Getting that association wrong silently attaches the
//! wrong nickname to the wrong mode, so the consumption rules live here on
//! their own, with tests, rather than being inlined into the command handler.

/// How many parameters a mode letter consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    /// Type A: a list mode such as `+b`. Always takes a parameter.
    List,
    /// Type B: always takes a parameter, setting or unsetting. Such as `+k`.
    Always,
    /// Type C: takes a parameter only when set. Such as `+l`.
    OnSet,
    /// Type D: never takes a parameter. Such as `+n`.
    Never,
    /// A membership prefix such as `+o`. Always takes a nickname.
    Prefix,
}

impl ModeKind {
    /// Whether this letter consumes a parameter in the given direction.
    #[must_use]
    pub fn takes_param(self, adding: bool) -> bool {
        match self {
            Self::List | Self::Always | Self::Prefix => true,
            Self::OnSet => adding,
            Self::Never => false,
        }
    }
}

/// How a channel mode letter behaves. Mirrors the advertised
/// `CHANMODES=b,k,l,imnst` plus the `PREFIX=(ov)@+` membership modes.
#[must_use]
pub fn channel_mode_kind(letter: u8) -> Option<ModeKind> {
    match letter {
        b'b' => Some(ModeKind::List),
        b'k' => Some(ModeKind::Always),
        b'l' => Some(ModeKind::OnSet),
        b'i' | b'm' | b'n' | b's' | b't' => Some(ModeKind::Never),
        b'o' | b'v' => Some(ModeKind::Prefix),
        _ => None,
    }
}

/// One requested mode change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeChange {
    /// Whether the mode is being set rather than cleared.
    pub adding: bool,
    /// The mode letter.
    pub letter: u8,
    /// The parameter, if this letter takes one in this direction.
    pub param: Option<Vec<u8>>,
}

/// The result of parsing a mode string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedModes {
    /// Changes in the order they were requested.
    pub changes: Vec<ModeChange>,
    /// Letters that are not recognised, in the order encountered.
    pub unknown: Vec<u8>,
}

/// Parse a mode specification and its parameters.
///
/// Unknown letters are collected rather than aborting the parse, so a client
/// asking for one bad mode among several still gets the good ones applied and
/// a specific complaint about the rest.
///
/// A letter that needs a parameter but has none left is dropped: applying it
/// with a missing or borrowed parameter would attach it to the wrong target.
#[must_use]
pub fn parse(
    spec: &[u8],
    params: &[&[u8]],
    kind_of: impl Fn(u8) -> Option<ModeKind>,
) -> ParsedModes {
    let mut parsed = ParsedModes::default();
    let mut next_param = params.iter();
    // The specification may omit a leading sign, in which case modes are added.
    let mut adding = true;

    for &letter in spec {
        match letter {
            b'+' => adding = true,
            b'-' => adding = false,
            _ => {
                let Some(kind) = kind_of(letter) else {
                    parsed.unknown.push(letter);
                    continue;
                };
                let param = if kind.takes_param(adding) {
                    match next_param.next() {
                        Some(p) => Some((*p).to_vec()),
                        // Out of parameters: drop the change rather than
                        // apply it to whatever comes next.
                        None => continue,
                    }
                } else {
                    None
                };
                parsed.changes.push(ModeChange {
                    adding,
                    letter,
                    param,
                });
            }
        }
    }
    parsed
}

/// Render applied changes back into a `MODE` string and parameter list.
///
/// Consecutive changes in the same direction share one sign, which is what
/// every other implementation emits and what clients expect to parse.
#[must_use]
pub fn render(changes: &[ModeChange]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut spec = Vec::with_capacity(changes.len() + 2);
    let mut params = Vec::new();
    let mut current_sign: Option<bool> = None;

    for change in changes {
        if current_sign != Some(change.adding) {
            spec.push(if change.adding { b'+' } else { b'-' });
            current_sign = Some(change.adding);
        }
        spec.push(change.letter);
        if let Some(param) = &change.param {
            params.push(param.clone());
        }
    }
    (spec, params)
}

#[cfg(test)]
mod tests {
    use super::{ModeChange, channel_mode_kind, parse, render};

    fn change(adding: bool, letter: u8, param: Option<&str>) -> ModeChange {
        ModeChange {
            adding,
            letter,
            param: param.map(|p| p.as_bytes().to_vec()),
        }
    }

    #[test]
    fn parameterless_modes_take_nothing() {
        let parsed = parse(b"+nt", &[], channel_mode_kind);
        assert_eq!(
            parsed.changes,
            vec![change(true, b'n', None), change(true, b't', None)]
        );
    }

    #[test]
    fn prefix_modes_consume_a_nickname_each() {
        let parsed = parse(b"+ov", &[b"alice", b"bob"], channel_mode_kind);
        assert_eq!(
            parsed.changes,
            vec![
                change(true, b'o', Some("alice")),
                change(true, b'v', Some("bob")),
            ]
        );
    }

    #[test]
    fn a_sign_applies_until_the_next_one() {
        let parsed = parse(b"+o-v+t", &[b"alice", b"bob"], channel_mode_kind);
        assert_eq!(
            parsed.changes,
            vec![
                change(true, b'o', Some("alice")),
                change(false, b'v', Some("bob")),
                change(true, b't', None),
            ]
        );
    }

    #[test]
    fn limit_takes_a_parameter_only_when_set() {
        let adding = parse(b"+l", &[b"20"], channel_mode_kind);
        assert_eq!(adding.changes, vec![change(true, b'l', Some("20"))]);

        // Unsetting +l takes no parameter, so the nickname after it belongs
        // to the +o, not to the -l.
        let removing = parse(b"-l+o", &[b"alice"], channel_mode_kind);
        assert_eq!(
            removing.changes,
            vec![change(false, b'l', None), change(true, b'o', Some("alice"))]
        );
    }

    #[test]
    fn key_takes_a_parameter_in_both_directions() {
        let parsed = parse(b"-k+o", &[b"secret", b"alice"], channel_mode_kind);
        assert_eq!(
            parsed.changes,
            vec![
                change(false, b'k', Some("secret")),
                change(true, b'o', Some("alice")),
            ]
        );
    }

    #[test]
    fn unknown_letters_are_collected_without_stopping_the_parse() {
        let parsed = parse(b"+nZt", &[], channel_mode_kind);
        assert_eq!(parsed.unknown, vec![b'Z']);
        assert_eq!(
            parsed.changes,
            vec![change(true, b'n', None), change(true, b't', None)]
        );
    }

    #[test]
    fn a_mode_with_no_parameter_left_is_dropped() {
        // +o with nothing to apply it to must not steal the next parameter or
        // be applied to nobody.
        let parsed = parse(b"+ot", &[], channel_mode_kind);
        assert_eq!(parsed.changes, vec![change(true, b't', None)]);
    }

    #[test]
    fn a_missing_leading_sign_means_add() {
        let parsed = parse(b"nt", &[], channel_mode_kind);
        assert_eq!(
            parsed.changes,
            vec![change(true, b'n', None), change(true, b't', None)]
        );
    }

    #[test]
    fn rendering_groups_runs_of_the_same_sign() {
        let changes = vec![
            change(true, b'o', Some("alice")),
            change(true, b'v', Some("bob")),
            change(false, b't', None),
            change(false, b'n', None),
            change(true, b'l', Some("20")),
        ];
        let (spec, params) = render(&changes);
        assert_eq!(spec, b"+ov-tn+l");
        assert_eq!(
            params,
            vec![b"alice".to_vec(), b"bob".to_vec(), b"20".to_vec()]
        );
    }

    #[test]
    fn rendering_round_trips_through_parsing() {
        let original = parse(b"+ov-t", &[b"alice", b"bob"], channel_mode_kind);
        let (spec, params) = render(&original.changes);
        let params: Vec<&[u8]> = params.iter().map(Vec::as_slice).collect();
        let reparsed = parse(&spec, &params, channel_mode_kind);
        assert_eq!(reparsed.changes, original.changes);
    }
}
