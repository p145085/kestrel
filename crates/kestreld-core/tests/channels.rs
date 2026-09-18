//! Channels, messaging, topics and departures.

mod common;

use common::{
    assert_not_received, assert_received, assert_silent, feed, feed_at, lines_to, numerics_to,
    register, test_server,
};
use kestrel_proto::numeric;
use kestreld_core::Action;

#[test]
fn joining_creates_the_channel_and_makes_the_creator_an_operator() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    let actions = feed(&mut server, alice, "JOIN #chan");

    assert_received(&actions, alice, ":alice!~alice@example.host JOIN #chan");
    // The creator holds @, otherwise a fresh channel has nobody who can run it.
    assert_received(&actions, alice, "= #chan :@alice");
    assert!(numerics_to(&actions, alice).contains(&numeric::RPL_ENDOFNAMES));

    let channel = server.channel(b"#chan").expect("channel should exist");
    assert!(channel.status(alice).unwrap().operator);
    assert_eq!(channel.len(), 1);
}

#[test]
fn later_joiners_are_not_operators_and_are_announced() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");

    feed(&mut server, alice, "JOIN #chan");
    let actions = feed(&mut server, bob, "JOIN #chan");

    // Everyone already in the channel sees the join, including the joiner.
    assert_received(&actions, alice, ":bob!~bob@example.host JOIN #chan");
    assert_received(&actions, bob, ":bob!~bob@example.host JOIN #chan");
    // The names list goes only to the joiner.
    assert_received(&actions, bob, "= #chan :@alice bob");
    assert_not_received(&actions, alice, "ENDOFNAMES");

    assert!(
        !server
            .channel(b"#chan")
            .unwrap()
            .status(bob)
            .unwrap()
            .operator
    );
}

#[test]
fn channel_names_are_matched_by_casemapping() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");

    feed(&mut server, alice, "JOIN #Chan");
    feed(&mut server, bob, "JOIN #cHAN");

    assert_eq!(server.channel_count(), 1, "these are the same channel");
    let channel = server.channel(b"#chan").unwrap();
    assert_eq!(channel.len(), 2);
    // The name keeps the case it was created with.
    assert_eq!(channel.name(), b"#Chan");
}

#[test]
fn joining_the_same_channel_twice_does_nothing() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    feed(&mut server, alice, "JOIN #chan");
    let actions = feed(&mut server, alice, "JOIN #chan");

    assert_silent(&actions, alice);
    assert_eq!(server.channel(b"#chan").unwrap().len(), 1);
}

#[test]
fn a_comma_separated_join_enters_every_channel() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    feed(&mut server, alice, "JOIN #one,#two,#three");

    assert_eq!(server.channel_count(), 3);
    assert_eq!(server.client(alice).unwrap().channels().len(), 3);
}

#[test]
fn malformed_channel_names_are_refused() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    for bad in ["chan", "!chan"] {
        let actions = feed(&mut server, alice, &format!("JOIN {bad}"));
        assert_eq!(
            numerics_to(&actions, alice),
            vec![numeric::ERR_NOSUCHCHANNEL],
            "{bad} should be refused"
        );
    }
    assert_eq!(server.channel_count(), 0);
}

// --- messaging -------------------------------------------------------------

#[test]
fn channel_messages_reach_members_but_not_the_sender() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "PRIVMSG #chan :hello everyone");

    assert_received(
        &actions,
        bob,
        ":alice!~alice@example.host PRIVMSG #chan :hello everyone",
    );
    assert_silent(&actions, alice);
}

#[test]
fn non_members_cannot_shout_into_a_channel() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let mallory = register(&mut server, "mallory");
    feed(&mut server, alice, "JOIN #chan");

    let actions = feed(&mut server, mallory, "PRIVMSG #chan :spam");

    // +n is on by default, so this is refused.
    assert_eq!(
        numerics_to(&actions, mallory),
        vec![numeric::ERR_CANNOTSENDTOCHAN]
    );
    assert_silent(&actions, alice);
}

#[test]
fn a_refused_notice_produces_no_reply_at_all() {
    // NOTICE must never trigger an automatic reply, or two automated clients
    // can bounce errors off each other indefinitely.
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let mallory = register(&mut server, "mallory");
    feed(&mut server, alice, "JOIN #chan");

    for line in [
        "NOTICE #chan :spam",        // blocked by +n
        "NOTICE #nonexistent :spam", // no such channel
        "NOTICE nobody :spam",       // no such nick
        "NOTICE #chan",              // no text
    ] {
        let actions = feed(&mut server, mallory, line);
        assert_silent(&actions, mallory);
    }
}

#[test]
fn private_messages_are_delivered_by_nickname() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");

    let actions = feed(&mut server, alice, "PRIVMSG bob :hello bob");

    assert_received(
        &actions,
        bob,
        ":alice!~alice@example.host PRIVMSG bob :hello bob",
    );
    assert_silent(&actions, alice);
}

#[test]
fn messaging_an_unknown_nickname_is_reported() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    let actions = feed(&mut server, alice, "PRIVMSG nobody :hello");
    assert_eq!(numerics_to(&actions, alice), vec![numeric::ERR_NOSUCHNICK]);
}

#[test]
fn messaging_an_away_user_reports_their_away_message() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");

    feed(&mut server, bob, "AWAY :back in five");
    let actions = feed(&mut server, alice, "PRIVMSG bob :hello");

    assert_received(&actions, bob, "PRIVMSG bob :hello");
    assert_received(&actions, alice, "301 alice bob :back in five");

    // Clearing it stops the notice.
    feed(&mut server, bob, "AWAY");
    let actions = feed(&mut server, alice, "PRIVMSG bob :hello again");
    assert_not_received(&actions, alice, "301");
}

// --- topics ----------------------------------------------------------------

#[test]
fn an_operator_can_set_the_topic_and_everyone_is_told() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed_at(&mut server, alice, "TOPIC #chan :new topic", 1_700_000_000);

    assert_received(&actions, alice, "TOPIC #chan :new topic");
    assert_received(&actions, bob, "TOPIC #chan :new topic");

    let topic = server.channel(b"#chan").unwrap().topic().unwrap();
    assert_eq!(topic.text, b"new topic");
    assert_eq!(topic.setter, b"alice!~alice@example.host");
    assert_eq!(topic.set_at, 1_700_000_000);
}

#[test]
fn plus_t_stops_ordinary_members_changing_the_topic() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, bob, "TOPIC #chan :hijacked");

    assert_eq!(
        numerics_to(&actions, bob),
        vec![numeric::ERR_CHANOPRIVSNEEDED]
    );
    assert!(server.channel(b"#chan").unwrap().topic().is_none());
}

#[test]
fn querying_a_topic_reports_whether_one_is_set() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    feed(&mut server, alice, "JOIN #chan");

    let actions = feed(&mut server, alice, "TOPIC #chan");
    assert_eq!(numerics_to(&actions, alice), vec![numeric::RPL_NOTOPIC]);

    feed(&mut server, alice, "TOPIC #chan :something");
    let actions = feed(&mut server, alice, "TOPIC #chan");
    assert_eq!(
        numerics_to(&actions, alice),
        vec![numeric::RPL_TOPIC, numeric::RPL_TOPICWHOTIME]
    );
}

#[test]
fn a_joiner_is_shown_the_existing_topic() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, alice, "TOPIC #chan :welcome");

    let bob = register(&mut server, "bob");
    let actions = feed(&mut server, bob, "JOIN #chan");

    assert_received(&actions, bob, "332 bob #chan :welcome");
}

// --- leaving ---------------------------------------------------------------

#[test]
fn parting_tells_the_channel_including_the_leaver() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, bob, "PART #chan :goodbye");

    assert_received(&actions, bob, ":bob!~bob@example.host PART #chan :goodbye");
    assert_received(
        &actions,
        alice,
        ":bob!~bob@example.host PART #chan :goodbye",
    );
    assert!(!server.channel(b"#chan").unwrap().contains(bob));
    assert!(server.client(bob).unwrap().channels().is_empty());
}

#[test]
fn parting_a_channel_you_are_not_in_is_reported() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");

    let actions = feed(&mut server, bob, "PART #chan");
    assert_eq!(numerics_to(&actions, bob), vec![numeric::ERR_NOTONCHANNEL]);
}

#[test]
fn the_last_member_leaving_removes_the_channel() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    feed(&mut server, alice, "JOIN #chan");
    assert_eq!(server.channel_count(), 1);

    feed(&mut server, alice, "PART #chan");
    assert_eq!(server.channel_count(), 0);
}

#[test]
fn quitting_tells_everyone_who_shares_a_channel_and_closes_the_connection() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    let carol = register(&mut server, "carol");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "QUIT :so long");

    assert_received(
        &actions,
        bob,
        ":alice!~alice@example.host QUIT :Quit: so long",
    );
    assert_silent(&actions, carol);
    assert!(
        actions.contains(&Action::Close { client: alice }),
        "the connection should be closed"
    );
    assert!(server.client(alice).is_none());
    // Bob is still there, so the channel survives with him alone in it.
    assert_eq!(server.channel(b"#chan").unwrap().len(), 1);
    assert!(!server.channel(b"#chan").unwrap().contains(alice));
}

#[test]
fn an_abrupt_disconnect_is_cleaned_up_like_a_quit() {
    // A dropped socket and a QUIT must leave identical state, or a ghost is
    // left sitting in the channel that nobody can remove.
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let mut actions = Vec::new();
    server.disconnect(alice, b"Connection reset by peer", &mut actions);

    assert_received(&actions, bob, "QUIT :Connection reset by peer");
    assert!(server.client(alice).is_none());
    assert!(!server.channel(b"#chan").unwrap().contains(alice));
    assert_eq!(server.channel(b"#chan").unwrap().len(), 1);
}

#[test]
fn a_quitters_nickname_becomes_available_again() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    feed(&mut server, alice, "QUIT");

    let newcomer = server.connect(b"example.host".to_vec());
    let actions = feed(&mut server, newcomer, "NICK alice");
    assert!(
        numerics_to(&actions, newcomer).is_empty(),
        "the nickname should be free again"
    );
}

// --- whois -----------------------------------------------------------------

#[test]
fn whois_reports_identity_and_shared_channels() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "WHOIS bob");
    let lines = lines_to(&actions, alice);

    assert_received(
        &actions,
        alice,
        "311 alice bob ~bob example.host * :bob the tester",
    );
    assert_received(&actions, alice, "319 alice bob :@#chan");
    assert!(
        lines.last().unwrap().contains("End of /WHOIS list"),
        "the reply should be terminated, got {lines:#?}"
    );
}

#[test]
fn whois_on_an_unknown_nickname_still_terminates() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    let actions = feed(&mut server, alice, "WHOIS nobody");
    assert_eq!(
        numerics_to(&actions, alice),
        vec![numeric::ERR_NOSUCHNICK, numeric::RPL_ENDOFWHOIS],
        "an unterminated WHOIS leaves clients waiting forever"
    );
}
