//! Parsing conformance tests.
//!
//! Cases are drawn from the IRCv3 `message-tags` specification and the
//! community `ircdocs/parser-tests` corpus, plus the edge cases that tend to
//! break real clients: runs of spaces, empty trailing parameters, and
//! backslash handling in tag values.

use kestrel_proto::{Message, ParseError};

fn parse(line: &[u8]) -> Message<'_> {
    Message::parse(line).expect("should parse")
}

#[test]
fn bare_command_and_params() {
    let m = parse(b"foo bar baz asdf");
    assert!(m.source().is_none());
    assert_eq!(m.command(), b"foo");
    assert_eq!(m.params(), [&b"bar"[..], b"baz", b"asdf"]);
}

#[test]
fn command_with_no_params() {
    let m = parse(b":coolguy AWAY");
    assert_eq!(m.command(), b"AWAY");
    assert!(m.params().is_empty());
}

#[test]
fn trailing_param_keeps_spaces() {
    let m = parse(b"foo bar baz :asdf quux");
    assert_eq!(m.params(), [&b"bar"[..], b"baz", b"asdf quux"]);
}

#[test]
fn trailing_param_may_be_empty() {
    let m = parse(b"foo bar baz :");
    assert_eq!(m.params(), [&b"bar"[..], b"baz", b""]);
}

#[test]
fn trailing_param_may_start_with_colon() {
    let m = parse(b"foo bar baz ::asdf");
    assert_eq!(m.params(), [&b"bar"[..], b"baz", b":asdf"]);
}

#[test]
fn trailing_param_preserves_its_own_trailing_space() {
    let m = parse(b":coolguy PRIVMSG bar :lol :) ");
    assert_eq!(m.param(1), Some(&b"lol :) "[..]));
}

#[test]
fn runs_of_spaces_between_sections_are_skipped() {
    let m = parse(b":coolguy   PRIVMSG   #chan   :hey");
    assert_eq!(m.command(), b"PRIVMSG");
    assert_eq!(m.params(), [&b"#chan"[..], b"hey"]);
}

#[test]
fn crlf_terminator_is_optional() {
    let with = parse(b"PING :token\r\n");
    let without = parse(b"PING :token");
    assert_eq!(with, without);
}

// --- sources ---------------------------------------------------------------

#[test]
fn full_user_mask_source() {
    let m = parse(b":nick!user@host PRIVMSG #chan :hi");
    let src = m.source().unwrap();
    assert_eq!(src.nick, b"nick");
    assert_eq!(src.user, Some(&b"user"[..]));
    assert_eq!(src.host, Some(&b"host"[..]));
    assert!(!src.is_server());
}

#[test]
fn nick_and_host_without_user() {
    let m = parse(b":nick@host PRIVMSG #chan :hi");
    let src = m.source().unwrap();
    assert_eq!(src.nick, b"nick");
    assert_eq!(src.user, None);
    assert_eq!(src.host, Some(&b"host"[..]));
}

#[test]
fn bare_server_name_source() {
    let m = parse(b":irc.example.org 001 nick :Welcome");
    let src = m.source().unwrap();
    assert_eq!(src.nick, b"irc.example.org");
    assert!(src.is_server());
}

// --- numerics --------------------------------------------------------------

#[test]
fn three_digit_command_is_a_numeric() {
    assert_eq!(parse(b":srv 001 nick :Welcome").numeric(), Some(1));
    assert_eq!(parse(b":srv 366 nick #c :End").numeric(), Some(366));
    assert_eq!(parse(b":srv 439 nick :Too fast").numeric(), Some(439));
}

#[test]
fn verbs_and_malformed_numerics_are_not_numerics() {
    assert_eq!(parse(b"PRIVMSG #c :x").numeric(), None);
    assert_eq!(parse(b":srv 01 nick :x").numeric(), None);
    assert_eq!(parse(b":srv 0a1 nick :x").numeric(), None);
}

#[test]
fn command_comparison_is_case_insensitive() {
    let m = parse(b"privmsg #chan :hi");
    assert!(m.is_command(b"PRIVMSG"));
    assert!(m.is_command(b"privmsg"));
    assert!(!m.is_command(b"NOTICE"));
}

// --- tags ------------------------------------------------------------------

#[test]
fn tags_with_and_without_values() {
    let m = parse(b"@a=b;c=32;k;rt=ql7 foo");
    assert_eq!(m.tags().len(), 4);
    assert_eq!(&*m.tag(b"a").unwrap().value(), b"b");
    assert_eq!(&*m.tag(b"c").unwrap().value(), b"32");
    assert_eq!(&*m.tag(b"k").unwrap().value(), b"");
    assert_eq!(&*m.tag(b"rt").unwrap().value(), b"ql7");
}

#[test]
fn empty_value_and_missing_value_are_equivalent() {
    let m = parse(b"@c;h=;a=b :quux ab cd");
    assert_eq!(&*m.tag(b"c").unwrap().value(), b"");
    assert_eq!(&*m.tag(b"h").unwrap().value(), b"");
    assert_eq!(&*m.tag(b"a").unwrap().value(), b"b");
}

#[test]
fn tag_escape_table() {
    let m = parse(br"@semi=a\:b;space=a\sb;back=a\\b;cr=a\rb;lf=a\nb PING");
    assert_eq!(&*m.tag(b"semi").unwrap().value(), b"a;b");
    assert_eq!(&*m.tag(b"space").unwrap().value(), b"a b");
    assert_eq!(&*m.tag(b"back").unwrap().value(), b"a\\b");
    assert_eq!(&*m.tag(b"cr").unwrap().value(), b"a\rb");
    assert_eq!(&*m.tag(b"lf").unwrap().value(), b"a\nb");
}

#[test]
fn backslash_before_unknown_char_keeps_the_char() {
    let m = parse(br"@x=a\qb PING");
    assert_eq!(&*m.tag(b"x").unwrap().value(), b"aqb");
}

#[test]
fn lone_trailing_backslash_is_dropped() {
    let m = parse(br"@x=abc\ PING");
    assert_eq!(&*m.tag(b"x").unwrap().value(), b"abc");
}

#[test]
fn malformed_tag_entries_are_skipped_not_fatal() {
    // Empty entries and an entry with an empty key; the valid tags survive.
    let m = parse(b"@;a=b;;=novalue;c=d PING");
    assert_eq!(m.tags().len(), 2);
    assert_eq!(&*m.tag(b"a").unwrap().value(), b"b");
    assert_eq!(&*m.tag(b"c").unwrap().value(), b"d");
}

#[test]
fn client_only_and_vendor_prefixes() {
    let m = parse(b"@+kestrel.chat/rtc=payload;time=2026-09-18T01:00:00Z TAGMSG #chan");

    let rtc = m.tag(b"+kestrel.chat/rtc").unwrap();
    assert!(rtc.is_client_only());
    assert_eq!(rtc.vendor(), Some(&b"kestrel.chat"[..]));
    assert_eq!(rtc.name(), b"rtc");
    assert_eq!(&*rtc.value(), b"payload");

    let time = m.tag(b"time").unwrap();
    assert!(!time.is_client_only());
    assert_eq!(time.vendor(), None);
    assert_eq!(time.name(), b"time");
}

#[test]
fn tags_source_command_and_params_together() {
    let m = parse(b"@time=2026-09-18T01:00:00Z :nick!u@h PRIVMSG #chan :hello world");
    assert_eq!(&*m.tag(b"time").unwrap().value(), b"2026-09-18T01:00:00Z");
    assert_eq!(m.source().unwrap().nick, b"nick");
    assert!(m.is_command(b"PRIVMSG"));
    assert_eq!(m.params(), [&b"#chan"[..], b"hello world"]);
}

// --- rejections ------------------------------------------------------------

#[test]
fn empty_lines_are_rejected() {
    assert_eq!(Message::parse(b""), Err(ParseError::Empty));
    assert_eq!(Message::parse(b"\r\n"), Err(ParseError::Empty));
}

#[test]
fn lines_without_a_command_are_rejected() {
    assert_eq!(Message::parse(b"@a=b"), Err(ParseError::MissingCommand));
    assert_eq!(Message::parse(b":source"), Err(ParseError::MissingCommand));
    assert_eq!(Message::parse(b":source "), Err(ParseError::MissingCommand));
    assert_eq!(
        Message::parse(b"@a=b :source "),
        Err(ParseError::MissingCommand)
    );
}

#[test]
fn invalid_utf8_does_not_break_parsing() {
    // Latin-1 bytes are still common on older networks.
    let m = parse(b":nick PRIVMSG #chan :caf\xe9 \xff\xfe");
    assert_eq!(m.param(1), Some(&b"caf\xe9 \xff\xfe"[..]));
    assert_eq!(m.command_str(), "PRIVMSG");
}
