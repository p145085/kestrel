//! IRCv3 capabilities, and what each one changes about what a client sees.

mod common;

use common::{
    assert_not_received, assert_received, assert_silent, feed, lines_to, register, test_server,
};
use kestreld_core::{ClientId, Server};

/// Register a client that has first enabled `caps`.
fn register_with_caps(server: &mut Server, nick: &str, caps: &str) -> ClientId {
    let id = server.connect(b"example.host".to_vec());
    feed(server, id, "CAP LS 302");
    feed(server, id, &format!("CAP REQ :{caps}"));
    feed(server, id, &format!("NICK {nick}"));
    feed(server, id, &format!("USER {nick} 0 * :{nick} the tester"));
    feed(server, id, "CAP END");
    assert!(server.client(id).unwrap().is_registered(), "{nick}");
    id
}

#[test]
fn cap_ls_lists_every_supported_capability() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    let actions = feed(&mut server, id, "CAP LS 302");

    let line = &lines_to(&actions, id)[0];
    for expected in [
        "sasl=",
        "account-notify",
        "account-tag",
        "away-notify",
        "echo-message",
        "extended-join",
        "invite-notify",
        "message-tags",
        "multi-prefix",
        "server-time",
        "setname",
    ] {
        assert!(line.contains(expected), "{expected} missing from {line}");
    }
}

// --- server-time -----------------------------------------------------------

#[test]
fn server_time_tags_messages_for_clients_that_asked() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register_with_caps(&mut server, "bob", "server-time");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "PRIVMSG #chan :hello");

    // The test server is pinned to 2025-09-18.
    assert_received(&actions, bob, "@time=2025-09-18T00:00:00.000Z");
    assert_received(&actions, bob, "PRIVMSG #chan :hello");
}

#[test]
fn clients_that_did_not_ask_get_no_time_tag() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "PRIVMSG #chan :hello");
    assert_not_received(&actions, bob, "@time=");
}

#[test]
fn a_quit_is_tagged_too() {
    // Teardown produces messages like any other path; a bouncer replaying
    // history needs them ordered with everything else.
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register_with_caps(&mut server, "bob", "server-time");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "QUIT :bye");
    assert_received(&actions, bob, "@time=");
    assert_received(&actions, bob, "QUIT :Quit: bye");
}

// --- account-tag -----------------------------------------------------------

#[test]
fn account_tag_names_the_account_behind_a_nickname() {
    let mut server = test_server();
    server.accounts_mut().register(b"alice", b"pw", 0).unwrap();

    let alice = register_with_caps(&mut server, "alice", "sasl");
    feed(&mut server, alice, "AUTHENTICATE PLAIN");
    // "\0alice\0pw" base64-encoded.
    feed(&mut server, alice, "AUTHENTICATE AGFsaWNlAHB3");
    assert_eq!(server.client(alice).unwrap().account(), Some(&b"alice"[..]));

    let bob = register_with_caps(&mut server, "bob", "account-tag");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "PRIVMSG #chan :hello");
    assert_received(&actions, bob, "account=alice");
}

#[test]
fn an_unauthenticated_sender_carries_no_account_tag() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register_with_caps(&mut server, "bob", "account-tag");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "PRIVMSG #chan :hello");
    assert_not_received(&actions, bob, "account=");
}

// --- echo-message ----------------------------------------------------------

#[test]
fn echo_message_returns_the_senders_own_message() {
    let mut server = test_server();
    let alice = register_with_caps(&mut server, "alice", "echo-message");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "PRIVMSG #chan :hello");
    assert_received(&actions, alice, "PRIVMSG #chan :hello");
    assert_received(&actions, bob, "PRIVMSG #chan :hello");
}

#[test]
fn echo_message_also_covers_private_messages() {
    let mut server = test_server();
    let alice = register_with_caps(&mut server, "alice", "echo-message");
    let bob = register(&mut server, "bob");

    let actions = feed(&mut server, alice, "PRIVMSG bob :hello");
    assert_received(&actions, alice, "PRIVMSG bob :hello");
    assert_received(&actions, bob, "PRIVMSG bob :hello");
}

#[test]
fn without_echo_message_the_sender_hears_nothing_back() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "PRIVMSG #chan :hello");
    assert_silent(&actions, alice);
}

// --- extended-join ---------------------------------------------------------

#[test]
fn extended_join_carries_the_account_and_realname() {
    let mut server = test_server();
    let watcher = register_with_caps(&mut server, "watcher", "extended-join");
    feed(&mut server, watcher, "JOIN #chan");

    let alice = register(&mut server, "alice");
    let actions = feed(&mut server, alice, "JOIN #chan");

    // Not logged in, so the account is `*`.
    assert_received(&actions, watcher, "JOIN #chan * :alice the tester");
}

#[test]
fn a_plain_client_sees_an_ordinary_join() {
    let mut server = test_server();
    let watcher = register(&mut server, "watcher");
    feed(&mut server, watcher, "JOIN #chan");

    let alice = register(&mut server, "alice");
    let actions = feed(&mut server, alice, "JOIN #chan");

    let lines = lines_to(&actions, watcher);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].ends_with("JOIN #chan"), "got {}", lines[0]);
}

// --- multi-prefix ----------------------------------------------------------

#[test]
fn multi_prefix_shows_every_prefix_a_member_holds() {
    let mut server = test_server();
    let alice = register_with_caps(&mut server, "alice", "multi-prefix");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");
    feed(&mut server, alice, "MODE #chan +v bob");
    feed(&mut server, alice, "MODE #chan +o bob");

    let actions = feed(&mut server, alice, "NAMES #chan");
    assert_received(&actions, alice, "@+bob");
}

#[test]
fn without_multi_prefix_only_the_highest_prefix_is_shown() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");
    feed(&mut server, alice, "MODE #chan +ov bob bob");

    let actions = feed(&mut server, alice, "NAMES #chan");
    assert_received(&actions, alice, "@bob");
    assert_not_received(&actions, alice, "@+bob");
}

// --- away-notify -----------------------------------------------------------

#[test]
fn away_notify_tells_channel_peers_who_asked() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let watcher = register_with_caps(&mut server, "watcher", "away-notify");
    let plain = register(&mut server, "plain");
    for id in [alice, watcher, plain] {
        feed(&mut server, id, "JOIN #chan");
    }

    let away = feed(&mut server, alice, "AWAY :lunch");
    assert_received(&away, watcher, "AWAY :lunch");
    assert_not_received(&away, plain, "AWAY");

    let back = feed(&mut server, alice, "AWAY");
    let line = lines_to(&back, watcher);
    assert_eq!(line.len(), 1);
    assert!(line[0].ends_with("AWAY"), "got {}", line[0]);
}

// --- account-notify --------------------------------------------------------

#[test]
fn account_notify_announces_a_login_to_channel_peers() {
    let mut server = test_server();
    server.accounts_mut().register(b"alice", b"pw", 0).unwrap();

    let alice = register_with_caps(&mut server, "alice", "sasl");
    let watcher = register_with_caps(&mut server, "watcher", "account-notify");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, watcher, "JOIN #chan");

    feed(&mut server, alice, "AUTHENTICATE PLAIN");
    let actions = feed(&mut server, alice, "AUTHENTICATE AGFsaWNlAHB3");

    assert_received(&actions, watcher, "ACCOUNT alice");
}

// --- setname ---------------------------------------------------------------

#[test]
fn setname_changes_the_realname_and_tells_peers_who_asked() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let watcher = register_with_caps(&mut server, "watcher", "setname");
    let plain = register(&mut server, "plain");
    for id in [alice, watcher, plain] {
        feed(&mut server, id, "JOIN #chan");
    }

    let actions = feed(&mut server, alice, "SETNAME :Alice Liddell");

    assert_eq!(server.client(alice).unwrap().realname(), b"Alice Liddell");
    // The client that made the change is always told, capability or not.
    assert_received(&actions, alice, "SETNAME :Alice Liddell");
    assert_received(&actions, watcher, "SETNAME :Alice Liddell");
    assert_not_received(&actions, plain, "SETNAME");
}

#[test]
fn an_empty_setname_is_refused_with_a_standard_reply() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let actions = feed(&mut server, alice, "SETNAME :");
    assert_received(&actions, alice, "FAIL SETNAME INVALID_REALNAME");
}

// --- message-tags and TAGMSG ----------------------------------------------

#[test]
fn tagmsg_reaches_only_clients_that_negotiated_message_tags() {
    let mut server = test_server();
    let alice = register_with_caps(&mut server, "alice", "message-tags");
    let tagged = register_with_caps(&mut server, "tagged", "message-tags");
    let plain = register(&mut server, "plain");
    for id in [alice, tagged, plain] {
        feed(&mut server, id, "JOIN #chan");
    }

    let actions = feed(&mut server, alice, "@+typing=active TAGMSG #chan");

    assert_received(&actions, tagged, "+typing=active");
    assert_received(&actions, tagged, "TAGMSG #chan");
    // A client without the capability must see nothing at all, not a blank line.
    assert_silent(&actions, plain);
}

#[test]
fn client_only_tags_are_relayed_on_ordinary_messages() {
    let mut server = test_server();
    let alice = register_with_caps(&mut server, "alice", "message-tags");
    let tagged = register_with_caps(&mut server, "tagged", "message-tags");
    let plain = register(&mut server, "plain");
    for id in [alice, tagged, plain] {
        feed(&mut server, id, "JOIN #chan");
    }

    let actions = feed(&mut server, alice, "@+draft/reply=abc PRIVMSG #chan :hello");

    assert_received(&actions, tagged, "+draft/reply=abc");
    // The message still arrives for everyone; only the tags are withheld.
    assert_received(&actions, plain, "PRIVMSG #chan :hello");
    assert_not_received(&actions, plain, "+draft/reply");
}

#[test]
fn a_tagmsg_with_no_client_tags_is_dropped() {
    let mut server = test_server();
    let alice = register_with_caps(&mut server, "alice", "message-tags");
    let tagged = register_with_caps(&mut server, "tagged", "message-tags");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, tagged, "JOIN #chan");

    let actions = feed(&mut server, alice, "TAGMSG #chan");
    assert_silent(&actions, tagged);
}

#[test]
fn a_private_tagmsg_reaches_a_capable_recipient() {
    let mut server = test_server();
    let alice = register_with_caps(&mut server, "alice", "message-tags");
    let bob = register_with_caps(&mut server, "bob", "message-tags");

    let actions = feed(&mut server, alice, "@+typing=active TAGMSG bob");
    assert_received(&actions, bob, "TAGMSG bob");
    assert_received(&actions, bob, "+typing=active");
}

// --- invite-notify ---------------------------------------------------------

#[test]
fn invite_notify_tells_operators_who_asked() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let op = register_with_caps(&mut server, "op", "invite-notify");
    let member = register_with_caps(&mut server, "member", "invite-notify");
    register(&mut server, "carol");

    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, op, "JOIN #chan");
    feed(&mut server, member, "JOIN #chan");
    feed(&mut server, alice, "MODE #chan +o op");

    let actions = feed(&mut server, alice, "INVITE carol #chan");

    assert_received(&actions, op, "INVITE carol #chan");
    // An ordinary member is not told: an invitation is not an announcement.
    assert_not_received(&actions, member, "INVITE");
}
