//! Serialisation tests, including the round-trip property that matters most:
//! anything we write must parse back into what we wrote.

use kestrel_proto::{Message, SerializeError, Source, tags};

fn rendered(msg: &Message<'_>) -> Vec<u8> {
    msg.to_vec().expect("should serialise")
}

#[test]
fn minimal_message() {
    let msg = Message::new(b"PING").with_param(b"token");
    assert_eq!(rendered(&msg), b"PING token\r\n");
}

#[test]
fn param_with_space_becomes_trailing() {
    let msg = Message::new(b"PRIVMSG")
        .with_param(b"#chan")
        .with_param(b"hello world");
    assert_eq!(rendered(&msg), b"PRIVMSG #chan :hello world\r\n");
}

#[test]
fn empty_final_param_becomes_trailing() {
    let msg = Message::new(b"PRIVMSG")
        .with_param(b"#chan")
        .with_param(b"");
    assert_eq!(rendered(&msg), b"PRIVMSG #chan :\r\n");
}

#[test]
fn final_param_starting_with_colon_becomes_trailing() {
    let msg = Message::new(b"PRIVMSG")
        .with_param(b"#chan")
        .with_param(b":-)");
    assert_eq!(rendered(&msg), b"PRIVMSG #chan ::-)\r\n");
}

#[test]
fn source_is_written_with_leading_colon() {
    let msg = Message::new(b"PRIVMSG")
        .with_source(Source::parse(b"nick!user@host"))
        .with_param(b"#chan")
        .with_param(b"hi");
    // `hi` needs no trailing form, so the minimal encoding is used.
    assert_eq!(rendered(&msg), b":nick!user@host PRIVMSG #chan hi\r\n");
}

#[test]
fn trailing_form_can_be_forced_for_free_text() {
    let msg = Message::new(b"PRIVMSG")
        .with_param(b"#chan")
        .with_trailing_param(b"hi");
    assert_eq!(rendered(&msg), b"PRIVMSG #chan :hi\r\n");
}

#[test]
fn forcing_trailing_form_does_not_change_content() {
    let minimal = Message::new(b"PRIVMSG")
        .with_param(b"#chan")
        .with_param(b"hi");
    let forced = Message::new(b"PRIVMSG")
        .with_param(b"#chan")
        .with_trailing_param(b"hi");
    assert_eq!(minimal, forced);
}

#[test]
fn reserialising_a_parsed_message_reproduces_the_original_bytes() {
    let lines: &[&[u8]] = &[
        b"PING token\r\n",
        b"PRIVMSG #chan hi\r\n",
        b"PRIVMSG #chan :hi\r\n",
        b"PRIVMSG #chan :hello world\r\n",
        b":nick!user@host PRIVMSG #chan :hi there\r\n",
        b"@a=1;b=2 :srv 001 nick :Welcome\r\n",
        b"PRIVMSG #chan :\r\n",
        b"PRIVMSG #chan ::-)\r\n",
    ];

    for line in lines {
        let parsed = Message::parse(line).expect("should parse");
        assert_eq!(
            parsed.to_vec().expect("should serialise"),
            *line,
            "re-serialising changed {line:?}"
        );
    }
}

#[test]
fn tags_are_written_before_the_source() {
    let msg = Message::new(b"TAGMSG")
        .with_raw_tag(b"+kestrel.chat/rtc", b"payload")
        .with_source(Source::parse(b"nick!u@h"))
        .with_param(b"#chan");
    assert_eq!(
        rendered(&msg),
        b"@+kestrel.chat/rtc=payload :nick!u@h TAGMSG #chan\r\n"
    );
}

#[test]
fn valueless_tag_omits_the_equals_sign() {
    let msg = Message::new(b"PING").with_raw_tag(b"flag", b"");
    assert_eq!(rendered(&msg), b"@flag PING\r\n");
}

#[test]
fn multiple_tags_are_semicolon_separated() {
    let msg = Message::new(b"PING")
        .with_raw_tag(b"a", b"1")
        .with_raw_tag(b"b", b"2");
    assert_eq!(rendered(&msg), b"@a=1;b=2 PING\r\n");
}

// --- rejections ------------------------------------------------------------

#[test]
fn non_final_param_needing_trailing_form_is_rejected() {
    let with_space = Message::new(b"CMD")
        .with_param(b"has space")
        .with_param(b"tail");
    assert_eq!(
        with_space.to_vec(),
        Err(SerializeError::ParamMustBeLast { index: 0 })
    );

    let empty = Message::new(b"CMD").with_param(b"").with_param(b"tail");
    assert_eq!(
        empty.to_vec(),
        Err(SerializeError::ParamMustBeLast { index: 0 })
    );

    let colon = Message::new(b"CMD").with_param(b":x").with_param(b"tail");
    assert_eq!(
        colon.to_vec(),
        Err(SerializeError::ParamMustBeLast { index: 0 })
    );
}

#[test]
fn params_containing_line_terminators_are_rejected() {
    // Otherwise this is command injection: a CRLF in a parameter would let a
    // remote-supplied string forge an entire extra protocol line.
    for (bad, index) in [(&b"a\rb"[..], 0), (b"a\nb", 0), (b"a\0b", 0)] {
        let msg = Message::new(b"PRIVMSG").with_param(bad);
        assert_eq!(
            msg.to_vec(),
            Err(SerializeError::ParamHasControlByte { index }),
            "should reject {bad:?}"
        );
    }
}

#[test]
fn empty_command_is_rejected() {
    assert_eq!(
        Message::new(b"").to_vec(),
        Err(SerializeError::EmptyCommand)
    );
}

// --- round trips -----------------------------------------------------------

#[test]
fn round_trip_preserves_messages() {
    let lines: &[&[u8]] = &[
        b"PING token",
        b"PRIVMSG #chan :hello world",
        b":nick!user@host PRIVMSG #chan :hello world",
        b":irc.example.org 001 nick :Welcome to the network",
        b"@time=2026-09-18T01:00:00Z :nick!u@h PRIVMSG #chan :hi",
        b"@a=1;b=2;c PING",
        b"PRIVMSG #chan ::-)",
        b"PRIVMSG #chan :",
        b":coolguy AWAY",
    ];

    for line in lines {
        // `written` is declared first so that it outlives both messages that
        // borrow from it; `Message` owns a `SmallVec`, so drop order matters.
        let written = Message::parse(line)
            .expect("should parse")
            .to_vec()
            .expect("should serialise");
        let first = Message::parse(line).expect("should parse");
        let second = Message::parse(&written).expect("should re-parse");
        assert_eq!(first, second, "round trip changed {line:?} -> {written:?}");
    }
}

#[test]
fn escaped_tag_values_survive_a_round_trip() {
    let raw = b"weird; value\\with\ttabs\r\nand newlines";
    let escaped = tags::escape(raw);

    let msg = Message::new(b"PING").with_raw_tag(b"x", &escaped);
    let written = msg.to_vec().expect("should serialise");

    let parsed = Message::parse(&written).expect("should parse");
    assert_eq!(&*parsed.tag(b"x").unwrap().value(), raw);
}

#[test]
fn escape_is_the_inverse_of_unescape() {
    for raw in [
        &b""[..],
        b"plain",
        b"has space",
        b"has;semi",
        b"has\\backslash",
        b"has\r\nnewlines",
        b"\\\\\\",
        b";;; ",
    ] {
        let escaped = tags::escape(raw);
        assert_eq!(&*tags::unescape(&escaped), raw, "failed for {raw:?}");
    }
}

#[test]
fn escaped_values_never_contain_a_tag_delimiter() {
    // This is the property that keeps a hostile tag value from breaking out of
    // its own tag and forging neighbouring tags.
    let hostile = b"a;b=c d\\;e";
    let escaped = tags::escape(hostile);
    assert!(!escaped.contains(&b';'));
    assert!(!escaped.contains(&b' '));
}
