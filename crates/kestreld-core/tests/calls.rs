//! The `CALL` command family. See `spec/rtc-over-irc.md`.

mod common;

use common::{assert_received, assert_silent, feed, lines_to, test_server};
use kestreld_core::{ClientId, Server, ServerConfig};

/// A server with calls enabled.
fn call_server() -> Server {
    Server::new(
        ServerConfig {
            server_name: b"test.server".to_vec(),
            calls_enabled: true,
            ..ServerConfig::default()
        },
        1_758_153_600,
    )
}

/// Register a client that has enabled the call capability.
fn caller(server: &mut Server, nick: &str) -> ClientId {
    let id = server.connect(b"example.host".to_vec());
    feed(server, id, "CAP LS 302");
    feed(server, id, "CAP REQ :kestrel.chat/rtc");
    feed(server, id, &format!("NICK {nick}"));
    feed(server, id, &format!("USER {nick} 0 * :{nick}"));
    feed(server, id, "CAP END");
    assert!(server.client(id).unwrap().is_registered(), "{nick}");
    id
}

/// Two clients in `#test`, both able to call.
fn two_in_channel() -> (Server, ClientId, ClientId) {
    let mut server = call_server();
    let alice = caller(&mut server, "alice");
    let bob = caller(&mut server, "bob");
    feed(&mut server, alice, "JOIN #test");
    feed(&mut server, bob, "JOIN #test");
    (server, alice, bob)
}

/// The call id from a `CALL ... STARTED` reply.
fn started_id(lines: &[String]) -> String {
    let line = lines
        .iter()
        .find(|l| l.contains("STARTED"))
        .expect("should have started a call");
    // ":server CALL <id> STARTED <target> <media>"
    line.split_whitespace().nth(2).expect("call id").to_owned()
}

// --- availability ----------------------------------------------------------

#[test]
fn the_capability_is_advertised_only_when_calls_are_enabled() {
    let mut off = test_server();
    let id = off.connect(b"example.host".to_vec());
    let actions = feed(&mut off, id, "CAP LS 302");
    assert!(
        !lines_to(&actions, id)[0].contains("kestrel.chat/rtc"),
        "a server that carries no calls must not offer to"
    );

    let mut on = call_server();
    let id = on.connect(b"example.host".to_vec());
    let actions = feed(&mut on, id, "CAP LS 302");
    assert_received(&actions, id, "kestrel.chat/rtc=ver=0");
}

#[test]
fn calls_are_refused_when_the_server_has_them_off() {
    let mut server = test_server();
    let alice = common::register(&mut server, "alice");
    feed(&mut server, alice, "JOIN #test");

    let actions = feed(&mut server, alice, "CALL START #test");
    assert_received(&actions, alice, "FAIL CALL RTC_DISABLED");
}

#[test]
fn a_client_without_the_capability_cannot_call() {
    // It would have no way to be told what happened next.
    let mut server = call_server();
    let alice = common::register(&mut server, "alice");
    feed(&mut server, alice, "JOIN #test");

    let actions = feed(&mut server, alice, "CALL START #test");
    assert_received(&actions, alice, "FAIL CALL RTC_DISABLED");
}

// --- starting --------------------------------------------------------------

#[test]
fn starting_a_call_in_a_channel_reports_it_and_tells_the_channel() {
    let (mut server, alice, _bob) = two_in_channel();
    let actions = feed(&mut server, alice, "CALL START #test");

    assert_received(&actions, alice, "STARTED #test audio,video");
    assert_eq!(server.calls().len(), 1);
    let call = server.calls().by_target(b"#test").unwrap();
    assert!(call.has_accepted(alice));
    assert_eq!(call.len(), 1);
}

#[test]
fn a_second_client_joins_the_existing_call_rather_than_making_another() {
    let (mut server, alice, bob) = two_in_channel();
    feed(&mut server, alice, "CALL START #test");

    let actions = feed(&mut server, bob, "CALL START #test");

    assert_eq!(server.calls().len(), 1, "one call per channel");
    assert_received(&actions, alice, "JOINED #test");
    assert_eq!(server.calls().by_target(b"#test").unwrap().len(), 2);
}

#[test]
fn requested_media_is_honoured() {
    let (mut server, alice, _bob) = two_in_channel();
    let actions = feed(&mut server, alice, "CALL START #test audio");
    assert_received(&actions, alice, "STARTED #test audio");
}

#[test]
fn calling_in_a_channel_you_are_not_in_is_refused() {
    // Channel permissions govern calls too: someone who may not speak there
    // may not call there.
    let (mut server, alice, _bob) = two_in_channel();
    let outsider = caller(&mut server, "outsider");
    feed(&mut server, alice, "CALL START #test");

    let actions = feed(&mut server, outsider, "CALL START #test");
    assert_received(&actions, outsider, "FAIL CALL NOT_IN_CHANNEL");
}

#[test]
fn calling_a_nonexistent_target_is_refused() {
    let (mut server, alice, _bob) = two_in_channel();
    for target in ["#nowhere", "nobody"] {
        let actions = feed(&mut server, alice, &format!("CALL START {target}"));
        assert_received(&actions, alice, "FAIL CALL NO_SUCH_TARGET");
    }
}

// --- one to one ------------------------------------------------------------

#[test]
fn calling_a_nickname_invites_rather_than_adds_them() {
    // Being called is not the same as answering.
    let (mut server, alice, bob) = two_in_channel();
    let actions = feed(&mut server, alice, "CALL START bob");

    assert_received(&actions, bob, "INVITE bob audio,video");
    let call = server.calls().by_target(b"bob").unwrap();
    assert!(call.contains(bob), "bob is invited");
    assert!(!call.has_accepted(bob), "but has not answered");
    assert_eq!(call.len(), 1, "only alice is actually in it");
}

#[test]
fn accepting_puts_you_in_the_call_and_tells_the_others() {
    let (mut server, alice, bob) = two_in_channel();
    let started = feed(&mut server, alice, "CALL START bob");
    let call_id = started_id(&lines_to(&started, alice));

    let actions = feed(&mut server, bob, &format!("CALL ACCEPT {call_id}"));

    assert_received(&actions, alice, "JOINED bob");
    assert!(server.calls().by_target(b"bob").unwrap().has_accepted(bob));
}

#[test]
fn declining_removes_you_and_tells_the_caller() {
    let (mut server, alice, bob) = two_in_channel();
    let started = feed(&mut server, alice, "CALL START bob");
    let call_id = started_id(&lines_to(&started, alice));

    let actions = feed(&mut server, bob, &format!("CALL DECLINE {call_id} :busy"));

    assert_received(&actions, alice, "DECLINED bob :busy");
    assert!(!server.calls().by_target(b"bob").unwrap().contains(bob));
}

#[test]
fn accepting_a_call_you_were_not_invited_to_is_refused() {
    let (mut server, alice, _bob) = two_in_channel();
    let carol = caller(&mut server, "carol");
    let started = feed(&mut server, alice, "CALL START bob");
    let call_id = started_id(&lines_to(&started, alice));

    let actions = feed(&mut server, carol, &format!("CALL ACCEPT {call_id}"));
    assert_received(&actions, carol, "FAIL CALL NOT_IN_CALL");
}

// --- leaving ---------------------------------------------------------------

#[test]
fn leaving_tells_whoever_is_left() {
    let (mut server, alice, bob) = two_in_channel();
    feed(&mut server, alice, "CALL START #test");
    let joined = feed(&mut server, bob, "CALL START #test");
    let call_id = started_id(&lines_to(&joined, bob));

    let actions = feed(&mut server, bob, &format!("CALL LEAVE {call_id}"));

    assert_received(&actions, alice, "LEFT #test");
    assert_eq!(server.calls().by_target(b"#test").unwrap().len(), 1);
}

#[test]
fn the_last_participant_leaving_ends_the_call() {
    let (mut server, alice, _bob) = two_in_channel();
    let started = feed(&mut server, alice, "CALL START #test");
    let call_id = started_id(&lines_to(&started, alice));

    feed(&mut server, alice, &format!("CALL LEAVE {call_id}"));
    assert!(server.calls().is_empty());
}

#[test]
fn quitting_the_server_leaves_every_call() {
    // A participant nobody can reach would otherwise sit in the roster forever.
    let (mut server, alice, bob) = two_in_channel();
    feed(&mut server, alice, "CALL START #test");
    feed(&mut server, bob, "CALL START #test");

    feed(&mut server, bob, "QUIT :bye");

    let call = server.calls().by_target(b"#test").unwrap();
    assert!(!call.contains(bob));
    assert_eq!(call.len(), 1);
}

#[test]
fn an_abrupt_disconnect_leaves_the_call_too() {
    let (mut server, alice, bob) = two_in_channel();
    feed(&mut server, alice, "CALL START #test");
    feed(&mut server, bob, "CALL START #test");

    let mut actions = Vec::new();
    server.disconnect(bob, b"Connection reset", &mut actions);

    assert!(!server.calls().by_target(b"#test").unwrap().contains(bob));
}

#[test]
fn leaving_a_call_you_are_not_in_is_refused() {
    let (mut server, alice, bob) = two_in_channel();
    let started = feed(&mut server, alice, "CALL START #test");
    let call_id = started_id(&lines_to(&started, alice));

    let actions = feed(&mut server, bob, &format!("CALL LEAVE {call_id}"));
    assert_received(&actions, bob, "FAIL CALL NOT_IN_CALL");
}

// --- discovery -------------------------------------------------------------

#[test]
fn listing_reports_the_call_and_who_is_in_it() {
    let (mut server, alice, bob) = two_in_channel();
    feed(&mut server, alice, "CALL START #test");
    feed(&mut server, bob, "CALL START #test");

    let actions = feed(&mut server, bob, "CALL LIST #test");

    assert_received(&actions, bob, "INFO #test audio,video 2");
    assert_received(&actions, bob, "PARTICIPANT #test alice *");
    assert_received(&actions, bob, "END #test :End of call list");
}

#[test]
fn listing_a_channel_you_are_not_in_is_refused() {
    // The participant list would otherwise disclose who is talking to whom.
    let (mut server, alice, _bob) = two_in_channel();
    let outsider = caller(&mut server, "outsider");
    feed(&mut server, alice, "CALL START #test");

    let actions = feed(&mut server, outsider, "CALL LIST #test");
    assert_received(&actions, outsider, "FAIL CALL NOT_IN_CHANNEL");
}

#[test]
fn listing_a_channel_with_no_call_still_terminates() {
    let (mut server, _alice, bob) = two_in_channel();
    let actions = feed(&mut server, bob, "CALL LIST #test");
    assert_received(&actions, bob, "END #test :End of call list");
}

// --- signalling ------------------------------------------------------------

#[test]
fn signalling_is_relayed_unchanged_to_the_named_peer() {
    let (mut server, alice, bob) = two_in_channel();
    feed(&mut server, alice, "CALL START #test");
    let joined = feed(&mut server, bob, "CALL START #test");
    let call_id = started_id(&lines_to(&joined, bob));

    let actions = feed(
        &mut server,
        alice,
        &format!("CALL SIGNAL {call_id} bob :c2VhbGVkLXBheWxvYWQ"),
    );

    assert_received(&actions, bob, "SIGNAL :c2VhbGVkLXBheWxvYWQ");
    assert_silent(&actions, alice);
}

#[test]
fn a_broadcast_signal_reaches_everyone_but_the_sender() {
    let (mut server, alice, bob) = two_in_channel();
    let carol = caller(&mut server, "carol");
    feed(&mut server, carol, "JOIN #test");
    feed(&mut server, alice, "CALL START #test");
    feed(&mut server, bob, "CALL START #test");
    let joined = feed(&mut server, carol, "CALL START #test");
    let call_id = started_id(&lines_to(&joined, carol));

    let actions = feed(
        &mut server,
        alice,
        &format!("CALL SIGNAL {call_id} * :cGF5"),
    );

    assert_received(&actions, bob, "SIGNAL :cGF5");
    assert_received(&actions, carol, "SIGNAL :cGF5");
    assert_silent(&actions, alice);
}

#[test]
fn only_participants_may_signal() {
    // Otherwise the relay is an open channel to anybody whose nick you know.
    let (mut server, alice, bob) = two_in_channel();
    let outsider = caller(&mut server, "outsider");
    feed(&mut server, alice, "CALL START #test");
    let joined = feed(&mut server, bob, "CALL START #test");
    let call_id = started_id(&lines_to(&joined, bob));

    let actions = feed(
        &mut server,
        outsider,
        &format!("CALL SIGNAL {call_id} alice :cGF5"),
    );
    assert_received(&actions, outsider, "FAIL CALL NOT_IN_CALL");
    assert_silent(&actions, alice);
}

#[test]
fn signalling_to_somebody_outside_the_call_is_refused() {
    let (mut server, alice, _bob) = two_in_channel();
    let outsider = caller(&mut server, "outsider");
    feed(&mut server, alice, "CALL START #test");
    let call_id = {
        let call = server.calls().by_target(b"#test").unwrap();
        String::from_utf8(call.id().to_wire()).unwrap()
    };

    let actions = feed(
        &mut server,
        alice,
        &format!("CALL SIGNAL {call_id} outsider :cGF5"),
    );
    assert_received(&actions, alice, "FAIL CALL NOT_IN_CALL");
    assert_silent(&actions, outsider);
}

#[test]
fn an_empty_or_oversized_payload_is_refused() {
    let (mut server, alice, bob) = two_in_channel();
    feed(&mut server, alice, "CALL START #test");
    let joined = feed(&mut server, bob, "CALL START #test");
    let call_id = started_id(&lines_to(&joined, bob));

    let empty = feed(&mut server, alice, &format!("CALL SIGNAL {call_id} bob :"));
    assert_received(&empty, alice, "FAIL CALL INVALID_PAYLOAD");

    let huge = "x".repeat(kestreld_core::rtc::MAX_SIGNAL + 1);
    let oversized = feed(
        &mut server,
        alice,
        &format!("CALL SIGNAL {call_id} bob :{huge}"),
    );
    assert_received(&oversized, alice, "FAIL CALL INVALID_PAYLOAD");
}

#[test]
fn an_unknown_call_id_is_refused() {
    let (mut server, alice, _bob) = two_in_channel();
    for bad in ["c999", "nonsense", "c"] {
        let actions = feed(&mut server, alice, &format!("CALL SIGNAL {bad} bob :cGF5"));
        assert_received(&actions, alice, "FAIL CALL NO_SUCH_CALL");
    }
}

#[test]
fn a_call_is_capped_at_the_mesh_limit() {
    // Above the cap the upload each participant needs stops being something an
    // ordinary connection can carry; refusing is more honest than admitting
    // somebody to a call that will not work.
    let mut server = call_server();
    let mut clients = Vec::new();
    for index in 0..=kestreld_core::rtc::MAX_MESH {
        let id = caller(&mut server, &format!("user{index}"));
        feed(&mut server, id, "JOIN #test");
        clients.push(id);
    }

    for &id in clients.iter().take(kestreld_core::rtc::MAX_MESH) {
        feed(&mut server, id, "CALL START #test");
    }
    assert_eq!(
        server.calls().by_target(b"#test").unwrap().len(),
        kestreld_core::rtc::MAX_MESH
    );

    let last = *clients.last().unwrap();
    let actions = feed(&mut server, last, "CALL START #test");
    assert_received(&actions, last, "FAIL CALL CALL_FULL");
}
