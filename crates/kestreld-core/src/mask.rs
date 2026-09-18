//! Matching `nick!user@host` masks against wildcard patterns.
//!
//! Ban and exception lists are patterns like `*!*@example.com`. The matcher
//! must not backtrack exponentially: patterns come from users, and a pattern
//! such as `*a*a*a*a*a*a*b` against a long host is a denial-of-service if the
//! implementation is the naive recursive one.

/// Whether `subject` matches `pattern`, treating `*` as any run of bytes and
/// `?` as exactly one.
///
/// Comparison is ASCII case-insensitive, as hostnames and nicknames are.
#[must_use]
pub fn matches(pattern: &[u8], subject: &[u8]) -> bool {
    let mut p = 0;
    let mut s = 0;
    // Where the most recent `*` was, and how much of the subject it had
    // consumed. Backtracking rewinds to here instead of recursing, which keeps
    // the whole match linear in practice.
    let mut star: Option<usize> = None;
    let mut star_consumed = 0;

    while s < subject.len() {
        if p < pattern.len() && (pattern[p] == b'?' || eq_ignore_case(pattern[p], subject[s])) {
            p += 1;
            s += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            star_consumed = s;
            p += 1;
        } else if let Some(star_pos) = star {
            // Let the star swallow one more byte and try again.
            p = star_pos + 1;
            star_consumed += 1;
            s = star_consumed;
        } else {
            return false;
        }
    }

    // Trailing stars may match nothing.
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn eq_ignore_case(a: u8, b: u8) -> bool {
    a.eq_ignore_ascii_case(&b)
}

/// Expand a user-supplied ban target into a full `nick!user@host` mask.
///
/// Users type `alice` or `*@example.com`; a list entry that is not a full mask
/// would never match anything, so the missing parts are filled with `*`.
#[must_use]
pub fn normalise(input: &[u8]) -> Vec<u8> {
    let has_bang = input.contains(&b'!');
    let has_at = input.contains(&b'@');

    match (has_bang, has_at) {
        (true, true) => input.to_vec(),
        // `user@host` — any nick.
        (false, true) => [b"*!", input].concat(),
        // `nick!user` — any host.
        (true, false) => [input, b"@*"].concat(),
        // A bare nick.
        (false, false) => [input, b"!*@*"].concat(),
    }
}

#[cfg(test)]
mod tests {
    use super::{matches, normalise};

    #[test]
    fn literal_patterns_match_exactly() {
        assert!(matches(b"alice!u@host", b"alice!u@host"));
        assert!(!matches(b"alice!u@host", b"bob!u@host"));
    }

    #[test]
    fn matching_ignores_ascii_case() {
        assert!(matches(b"Alice!*@*", b"alice!u@host"));
        assert!(matches(b"*!*@EXAMPLE.COM", b"bob!u@example.com"));
    }

    #[test]
    fn star_matches_any_run_including_nothing() {
        assert!(matches(b"*", b""));
        assert!(matches(b"*", b"anything"));
        assert!(matches(b"a*c", b"ac"));
        assert!(matches(b"a*c", b"abc"));
        assert!(matches(b"a*c", b"abbbbc"));
        assert!(!matches(b"a*c", b"abd"));
    }

    #[test]
    fn question_mark_matches_exactly_one_byte() {
        assert!(matches(b"a?c", b"abc"));
        assert!(!matches(b"a?c", b"ac"));
        assert!(!matches(b"a?c", b"abbc"));
    }

    #[test]
    fn typical_ban_masks_work() {
        let subject = b"alice!~alice@host.example.com";
        assert!(matches(b"*!*@*.example.com", subject));
        assert!(matches(b"alice!*@*", subject));
        assert!(matches(b"*!~alice@*", subject));
        assert!(!matches(b"bob!*@*", subject));
        assert!(!matches(b"*!*@*.example.org", subject));
    }

    #[test]
    fn consecutive_stars_are_harmless() {
        assert!(matches(b"**a**b**", b"xxaybz"));
    }

    #[test]
    fn pathological_patterns_terminate_promptly() {
        // The naive recursive matcher takes exponential time on this; the
        // iterative one must not.
        let pattern = b"*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b";
        let subject = vec![b'a'; 512];
        assert!(!matches(pattern, &subject));
    }

    #[test]
    fn partial_masks_are_expanded_to_full_ones() {
        assert_eq!(normalise(b"alice"), b"alice!*@*");
        assert_eq!(normalise(b"alice!user"), b"alice!user@*");
        assert_eq!(normalise(b"user@host"), b"*!user@host");
        assert_eq!(normalise(b"alice!user@host"), b"alice!user@host");
        assert_eq!(normalise(b"*@example.com"), b"*!*@example.com");
    }

    #[test]
    fn a_normalised_bare_nick_matches_that_users_mask() {
        let mask = normalise(b"alice");
        assert!(matches(&mask, b"alice!~alice@example.host"));
        assert!(!matches(&mask, b"bob!~bob@example.host"));
    }
}
