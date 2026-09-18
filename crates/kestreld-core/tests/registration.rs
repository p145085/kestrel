//! Registration, capability negotiation, and nickname handling.

mod common;

use common::{all_lines, assert_received, feed, lines_to, numerics_to, register, test_server};
use kestrel_proto::numeric;
use kestreld_core::{Server, ServerConfig};

#[test]
fn nick_then_user_completes_registration() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());

    let after_nick = feed(&mut server, id, "NICK alice");
    assert!(
        after_nick.is_empty(),
        "NICK alone should not complete registration"
    );
    assert!(!server.client(id).unwrap().is_registered());

    let after_user = feed(&mut server, id, "USER alice 0 * :Alice");
    assert!(server.client(id).unwrap().is_registered());

    let codes = numerics_to(&after_user, id);
    for expected in [
        numeric::RPL_WELCOME,
        numeric::RPL_YOURHOST,
        numeric::RPL_CREATED,
        numeric::RPL_MYINFO,
        numeric::RPL_ISUPPORT,
    ] {
        assert!(
            codes.contains(&expected),
            "missing numeric {expected} in {codes:?}"
        );
    }
}

#[test]
fn user_before_nick_also_registers() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());

    feed(&mut server, id, "USER alice 0 * :Alice");
    assert!(!server.client(id).unwrap().is_registered());

    let actions = feed(&mut server, id, "NICK alice");
    assert!(server.client(id).unwrap().is_registered());
    assert!(numerics_to(&actions, id).contains(&numeric::RPL_WELCOME));
}

#[test]
fn welcome_reports_the_configured_network_and_version() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    feed(&mut server, id, "NICK alice");
    let actions = feed(&mut server, id, "USER alice 0 * :Alice");

    assert_received(&actions, id, "Welcome to the TestNet IRC Network");
    assert_received(&actions, id, "running version kestreld-test");
    // The creation date is rendered from the timestamp the server was given.
    assert_received(
        &actions,
        id,
        "This server was created 2025-09-18 00:00:00 UTC",
    );
}

#[test]
fn isupport_advertises_the_servers_actual_limits() {
    let mut server = Server::new(
        ServerConfig {
            max_nick_len: 16,
            max_channel_len: 50,
            network_name: b"Limited".to_vec(),
            ..ServerConfig::default()
        },
        0,
    );
    let id = server.connect(b"example.host".to_vec());
    feed(&mut server, id, "NICK alice");
    let actions = feed(&mut server, id, "USER alice 0 * :Alice");

    assert_received(&actions, id, "NICKLEN=16");
    assert_received(&actions, id, "CHANNELLEN=50");
    assert_received(&actions, id, "NETWORK=Limited");
    assert_received(&actions, id, "CASEMAPPING=rfc1459");
    assert_received(&actions, id, "PREFIX=(ov)@+");
}

#[test]
fn commands_are_refused_before_registration() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());

    for line in [
        "JOIN #chan",
        "PRIVMSG #chan :hi",
        "TOPIC #chan",
        "WHOIS bob",
    ] {
        let actions = feed(&mut server, id, line);
        assert_eq!(
            numerics_to(&actions, id),
            vec![numeric::ERR_NOTREGISTERED],
            "{line} should be refused before registration"
        );
    }
}

#[test]
fn cap_negotiation_holds_registration_open_until_cap_end() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());

    let ls = feed(&mut server, id, "CAP LS 302");
    assert_received(&ls, id, "CAP * LS");

    feed(&mut server, id, "NICK alice");
    let after_user = feed(&mut server, id, "USER alice 0 * :Alice");
    assert!(
        !server.client(id).unwrap().is_registered(),
        "registration must wait for CAP END"
    );
    assert!(numerics_to(&after_user, id).is_empty());

    let after_end = feed(&mut server, id, "CAP END");
    assert!(server.client(id).unwrap().is_registered());
    assert!(numerics_to(&after_end, id).contains(&numeric::RPL_WELCOME));
}

#[test]
fn unavailable_capabilities_are_refused_not_ignored() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    let actions = feed(&mut server, id, "CAP REQ :message-tags sasl");
    // NAK must echo the request verbatim so the client knows what was refused.
    assert_received(&actions, id, "CAP * NAK :message-tags sasl");
}

#[test]
fn a_taken_nickname_is_refused() {
    let mut server = test_server();
    register(&mut server, "alice");

    let bob = server.connect(b"example.host".to_vec());
    let actions = feed(&mut server, bob, "NICK alice");
    assert_eq!(numerics_to(&actions, bob), vec![numeric::ERR_NICKNAMEINUSE]);
}

#[test]
fn nickname_collision_honours_casemapping() {
    let mut server = test_server();
    register(&mut server, "alice");

    let bob = server.connect(b"example.host".to_vec());
    // Different case, and an rfc1459 pair: both are the same nick.
    for attempt in ["ALICE", "Alice"] {
        let actions = feed(&mut server, bob, &format!("NICK {attempt}"));
        assert_eq!(
            numerics_to(&actions, bob),
            vec![numeric::ERR_NICKNAMEINUSE],
            "{attempt} should collide with alice"
        );
    }
}

#[test]
fn rfc1459_bracket_pairs_collide() {
    let mut server = test_server();
    register(&mut server, "nick[x]");

    let other = server.connect(b"example.host".to_vec());
    let actions = feed(&mut server, other, "NICK nick{x}");
    assert_eq!(
        numerics_to(&actions, other),
        vec![numeric::ERR_NICKNAMEINUSE],
        "on rfc1459 networks nick[x] and nick{{x}} are the same nickname"
    );
}

#[test]
fn malformed_nicknames_are_refused() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());

    for bad in ["#chan", "1alice", "-alice", "@op"] {
        let actions = feed(&mut server, id, &format!("NICK {bad}"));
        assert_eq!(
            numerics_to(&actions, id),
            vec![numeric::ERR_ERRONEUSNICKNAME],
            "{bad} should be refused"
        );
    }
}

#[test]
fn changing_case_of_your_own_nick_is_allowed() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    let actions = feed(&mut server, alice, "NICK Alice");
    assert!(
        numerics_to(&actions, alice).is_empty(),
        "renaming yourself should not be a collision"
    );
    assert_received(&actions, alice, "NICK Alice");
    assert_eq!(server.client(alice).unwrap().nick(), Some(&b"Alice"[..]));
}

#[test]
fn nick_changes_reach_everyone_who_shares_a_channel() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    let carol = register(&mut server, "carol");

    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");
    // Carol shares no channel, so she must not be told.

    let actions = feed(&mut server, alice, "NICK alice2");
    assert_received(&actions, alice, ":alice!~alice@example.host NICK alice2");
    assert_received(&actions, bob, ":alice!~alice@example.host NICK alice2");
    assert!(
        lines_to(&actions, carol).is_empty(),
        "carol shares no channel and should not see the change"
    );
}

#[test]
fn registering_twice_is_refused() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    let actions = feed(&mut server, alice, "USER other 0 * :Other");
    assert_eq!(
        numerics_to(&actions, alice),
        vec![numeric::ERR_ALREADYREGISTERED]
    );
}

#[test]
fn a_required_password_gates_registration() {
    let mut server = Server::new(
        ServerConfig {
            password: Some(b"hunter2".to_vec()),
            ..ServerConfig::default()
        },
        0,
    );

    let wrong = server.connect(b"example.host".to_vec());
    feed(&mut server, wrong, "PASS wrong");
    feed(&mut server, wrong, "NICK alice");
    feed(&mut server, wrong, "USER alice 0 * :Alice");
    assert!(
        !server.client(wrong).unwrap().is_registered(),
        "a wrong password must not register"
    );

    let right = server.connect(b"example.host".to_vec());
    feed(&mut server, right, "PASS hunter2");
    feed(&mut server, right, "NICK bob");
    feed(&mut server, right, "USER bob 0 * :Bob");
    assert!(server.client(right).unwrap().is_registered());
}

#[test]
fn username_is_marked_as_unverified() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    // No ident lookup is done, so the username carries a `~` to say so.
    assert_eq!(server.client(alice).unwrap().user(), Some(&b"~alice"[..]));
    assert_eq!(
        server.client(alice).unwrap().mask(),
        b"alice!~alice@example.host"
    );
}

#[test]
fn ping_is_answered_with_the_same_token() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let actions = feed(&mut server, alice, "PING :abc123");
    assert_received(&actions, alice, "PONG test.server :abc123");
}

#[test]
fn unknown_commands_are_reported() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let actions = feed(&mut server, alice, "FLOOP a b c");
    assert_eq!(
        numerics_to(&actions, alice),
        vec![numeric::ERR_UNKNOWNCOMMAND]
    );
    assert_received(&actions, alice, "FLOOP :Unknown command");
}

#[test]
fn every_emitted_line_is_within_the_protocol_limit() {
    // A line over 512 bytes is silently truncated by real clients, which turns
    // a cosmetic bug into a protocol desync.
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    feed(&mut server, id, "NICK alice");
    let actions = feed(&mut server, id, "USER alice 0 * :Alice");

    for line in all_lines(&actions) {
        assert!(
            line.len() + 2 <= kestrel_proto::limits::MAX_LINE,
            "line exceeds the 512-byte limit ({} bytes): {line}",
            line.len() + 2
        );
    }
}
