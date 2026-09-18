//! The client session state machine.

use kestrel_proto::Message;
use kestrel_session::{Action, Event, MessageKind, Outcome, Sasl, Session, SessionConfig, Target};

/// Feed one line from the server.
fn feed(session: &mut Session, line: &str) -> Outcome {
    let bytes = line.as_bytes().to_vec();
    let msg = Message::parse(&bytes).expect("test line should parse");
    session.handle(&msg)
}

/// Everything the session decided to send, rendered as text.
fn sent(outcome: &Outcome) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            Action::Send(message) => Some(
                String::from_utf8_lossy(&message.to_vec().expect("should serialise"))
                    .trim_end_matches("\r\n")
                    .to_owned(),
            ),
            Action::Disconnect => None,
        })
        .collect()
}

fn assert_sent(outcome: &Outcome, needle: &str) {
    let lines = sent(outcome);
    assert!(
        lines.iter().any(|l| l.contains(needle)),
        "expected to send something containing {needle:?}, sent {lines:#?}"
    );
}

fn assert_not_sent(outcome: &Outcome, needle: &str) {
    let lines = sent(outcome);
    assert!(
        !lines.iter().any(|l| l.contains(needle)),
        "did not expect to send {needle:?}, sent {lines:#?}"
    );
}

/// A session driven all the way to registered, with `alice` in `#test`.
fn registered_session() -> Session {
    let mut session = Session::new(SessionConfig::new("alice"));
    session.start();
    feed(&mut session, ":srv CAP * LS :multi-prefix server-time");
    feed(&mut session, ":srv CAP * ACK :multi-prefix server-time");
    feed(&mut session, ":srv 001 alice :Welcome");
    feed(
        &mut session,
        ":srv 005 alice PREFIX=(ov)@+ CHANTYPES=# CASEMAPPING=rfc1459 :are supported",
    );
    session
}

/// Put `alice` into `#test` alongside `bob` and `carol`.
fn in_channel() -> Session {
    let mut session = registered_session();
    feed(&mut session, ":alice!u@h JOIN #test");
    feed(&mut session, ":srv 353 alice = #test :@alice bob +carol");
    feed(&mut session, ":srv 366 alice #test :End of /NAMES list");
    session
}

// --- registration ----------------------------------------------------------

#[test]
fn starting_negotiates_capabilities_before_identifying() {
    let mut session = Session::new(SessionConfig::new("alice").with_realname("Alice"));
    let outcome = session.start();

    let lines = sent(&outcome);
    assert_eq!(lines[0], "CAP LS 302", "CAP LS must come first");
    assert_sent(&outcome, "NICK alice");
    assert_sent(&outcome, "USER alice 0 * :Alice");
    assert!(!session.is_registered());
}

#[test]
fn a_server_password_is_sent_before_the_nickname() {
    let mut config = SessionConfig::new("alice");
    config.server_password = Some(b"hunter2".to_vec());
    let mut session = Session::new(config);

    let lines = sent(&session.start());
    let pass = lines.iter().position(|l| l.starts_with("PASS")).unwrap();
    let nick = lines.iter().position(|l| l.starts_with("NICK")).unwrap();
    assert!(pass < nick, "PASS must precede NICK, got {lines:#?}");
}

#[test]
fn only_capabilities_the_server_offers_are_requested() {
    let mut session = Session::new(SessionConfig::new("alice"));
    session.start();

    let outcome = feed(
        &mut session,
        ":srv CAP * LS :multi-prefix server-time nonsense",
    );
    let requested = sent(&outcome).join(" ");
    assert!(requested.contains("multi-prefix"));
    assert!(requested.contains("server-time"));
    assert!(
        !requested.contains("nonsense"),
        "asking for something we never wanted: {requested}"
    );
    assert!(
        !requested.contains("sasl"),
        "sasl was not offered: {requested}"
    );
}

#[test]
fn a_multi_part_capability_listing_is_gathered_before_requesting() {
    // A request sent after the first batch would miss everything in the second.
    let mut session = Session::new(SessionConfig::new("alice"));
    session.start();

    let first = feed(&mut session, ":srv CAP * LS * :multi-prefix");
    assert_not_sent(&first, "CAP REQ");

    let second = feed(&mut session, ":srv CAP * LS :server-time");
    let requested = sent(&second).join(" ");
    assert!(requested.contains("multi-prefix"), "got {requested}");
    assert!(requested.contains("server-time"), "got {requested}");
}

#[test]
fn acknowledged_capabilities_end_negotiation() {
    let mut session = Session::new(SessionConfig::new("alice"));
    session.start();
    feed(&mut session, ":srv CAP * LS :multi-prefix");

    let outcome = feed(&mut session, ":srv CAP * ACK :multi-prefix");
    assert_sent(&outcome, "CAP END");
    assert!(session.enabled_caps().contains("multi-prefix"));
    assert!(
        outcome
            .events
            .iter()
            .any(|e| matches!(e, Event::CapsEnabled(_)))
    );
}

#[test]
fn a_refusal_does_not_stall_registration() {
    // Capabilities are all optional; a NAK must not leave the client waiting.
    let mut session = Session::new(SessionConfig::new("alice"));
    session.start();
    feed(&mut session, ":srv CAP * LS :multi-prefix");

    let outcome = feed(&mut session, ":srv CAP * NAK :multi-prefix");
    assert_sent(&outcome, "CAP END");
}

#[test]
fn a_server_without_capabilities_needs_no_cap_end() {
    let mut session = Session::new(SessionConfig::new("alice"));
    session.start();
    let outcome = feed(&mut session, ":srv CAP * LS :");
    assert_not_sent(&outcome, "CAP END");
}

#[test]
fn welcome_completes_registration_and_joins_configured_channels() {
    let mut session = Session::new(SessionConfig::new("alice").with_autojoin(["#one", "#two"]));
    session.start();

    let outcome = feed(&mut session, ":srv 001 alice :Welcome to the network");

    assert!(session.is_registered());
    assert_eq!(
        outcome.events[0],
        Event::Registered {
            nick: b"alice".to_vec()
        }
    );
    assert_sent(&outcome, "JOIN #one");
    assert_sent(&outcome, "JOIN #two");
}

#[test]
fn the_servers_idea_of_our_nickname_wins() {
    // A server may truncate or rewrite what we asked for; believing our own
    // version means every later comparison against ourselves is wrong.
    let mut session = Session::new(SessionConfig::new("averyverylongnickname"));
    session.start();
    feed(&mut session, ":srv 001 alic :Welcome");
    assert_eq!(session.nick(), b"alic");
    assert!(session.is_me(b"ALIC"));
}

#[test]
fn a_taken_nickname_falls_back_to_the_alternatives() {
    let mut config = SessionConfig::new("alice");
    config.alt_nicks = vec![b"alice2".to_vec(), b"alice3".to_vec()];
    let mut session = Session::new(config);
    session.start();

    let first = feed(&mut session, ":srv 433 * alice :Nickname is already in use");
    assert_sent(&first, "NICK alice2");

    let second = feed(
        &mut session,
        ":srv 433 * alice2 :Nickname is already in use",
    );
    assert_sent(&second, "NICK alice3");
}

#[test]
fn exhausted_alternatives_fall_back_to_underscores() {
    let mut session = Session::new(SessionConfig::new("alice"));
    session.start();

    let outcome = feed(&mut session, ":srv 433 * alice :Nickname is already in use");
    assert_sent(&outcome, "NICK alice_");

    let again = feed(
        &mut session,
        ":srv 433 * alice_ :Nickname is already in use",
    );
    assert_sent(&again, "NICK alice__");
}

#[test]
fn a_collision_after_registration_is_reported_not_retried() {
    // Once registered, 433 means a NICK the user asked for was refused, and
    // silently picking a different name would be worse than saying so.
    let mut session = registered_session();
    let outcome = feed(
        &mut session,
        ":srv 433 alice bob :Nickname is already in use",
    );

    assert_not_sent(&outcome, "NICK");
    assert!(
        outcome
            .events
            .iter()
            .any(|e| matches!(e, Event::Numeric { code: 433, .. }))
    );
}

#[test]
fn giving_up_ends_the_session_rather_than_looping() {
    let mut session = Session::new(SessionConfig::new("alice"));
    session.start();

    let mut proposed = Vec::new();
    let mut gave_up = false;
    for _ in 0..12 {
        let outcome = feed(&mut session, ":srv 433 * alice :Nickname is already in use");
        proposed.extend(sent(&outcome));
        gave_up |= outcome.actions.contains(&Action::Disconnect);
    }
    assert!(
        gave_up,
        "the session should stop trying, proposed {proposed:#?}"
    );

    // Every attempt must be a different name; repeating one the server already
    // refused would spin forever against a network with a short nick limit.
    let nicks: Vec<&String> = proposed.iter().filter(|l| l.starts_with("NICK ")).collect();
    let mut unique = nicks.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        nicks.len(),
        unique.len(),
        "a nickname was proposed twice: {nicks:#?}"
    );
}

// --- SASL ------------------------------------------------------------------

#[test]
fn sasl_runs_before_cap_end() {
    let mut session = Session::new(SessionConfig::new("alice").with_sasl(Sasl::Plain {
        account: b"alice".to_vec(),
        password: b"hunter2".to_vec(),
    }));
    session.start();
    feed(&mut session, ":srv CAP * LS :sasl=PLAIN,EXTERNAL");

    let ack = feed(&mut session, ":srv CAP * ACK :sasl");
    assert_sent(&ack, "AUTHENTICATE PLAIN");
    assert_not_sent(&ack, "CAP END");

    let challenge = feed(&mut session, "AUTHENTICATE +");
    // base64 of "\0alice\0hunter2"
    assert_sent(&challenge, "AUTHENTICATE AGFsaWNlAGh1bnRlcjI=");

    feed(
        &mut session,
        ":srv 900 alice alice!u@h alice :You are now logged in",
    );
    let success = feed(&mut session, ":srv 903 alice :SASL successful");
    assert_sent(&success, "CAP END");
    assert_eq!(session.account(), Some(&b"alice"[..]));
    assert!(
        success
            .events
            .iter()
            .any(|e| matches!(e, Event::LoggedIn { .. }))
    );
}

#[test]
fn external_sends_an_empty_response() {
    let mut session = Session::new(SessionConfig::new("alice").with_sasl(Sasl::External));
    session.start();
    feed(&mut session, ":srv CAP * LS :sasl=EXTERNAL");
    let ack = feed(&mut session, ":srv CAP * ACK :sasl");
    assert_sent(&ack, "AUTHENTICATE EXTERNAL");

    let challenge = feed(&mut session, "AUTHENTICATE +");
    assert_sent(&challenge, "AUTHENTICATE +");
}

#[test]
fn a_failed_login_still_lets_the_connection_proceed() {
    // Losing the connection over a bad password would be a worse outcome than
    // being connected without an account.
    let mut session = Session::new(SessionConfig::new("alice").with_sasl(Sasl::Plain {
        account: b"alice".to_vec(),
        password: b"wrong".to_vec(),
    }));
    session.start();
    feed(&mut session, ":srv CAP * LS :sasl=PLAIN");
    feed(&mut session, ":srv CAP * ACK :sasl");
    feed(&mut session, "AUTHENTICATE +");

    let outcome = feed(&mut session, ":srv 904 alice :SASL authentication failed");
    assert_sent(&outcome, "CAP END");
    assert!(
        outcome
            .events
            .iter()
            .any(|e| matches!(e, Event::LoginFailed { .. }))
    );
}

#[test]
fn sasl_is_skipped_when_the_server_does_not_offer_it() {
    let mut session = Session::new(SessionConfig::new("alice").with_sasl(Sasl::Plain {
        account: b"alice".to_vec(),
        password: b"hunter2".to_vec(),
    }));
    session.start();
    feed(&mut session, ":srv CAP * LS :multi-prefix");
    let outcome = feed(&mut session, ":srv CAP * ACK :multi-prefix");

    assert_not_sent(&outcome, "AUTHENTICATE");
    assert_sent(&outcome, "CAP END");
}

// --- housekeeping ----------------------------------------------------------

#[test]
fn a_ping_is_answered_with_its_token() {
    let mut session = registered_session();
    let outcome = feed(&mut session, "PING :abc123");
    assert_sent(&outcome, "PONG :abc123");
}

#[test]
fn isupport_changes_how_names_are_compared() {
    let mut session = registered_session();
    assert!(session.isupport().eq(b"nick[x]", b"nick{x}"));

    feed(
        &mut session,
        ":srv 005 alice CASEMAPPING=ascii :are supported",
    );
    assert!(!session.isupport().eq(b"nick[x]", b"nick{x}"));
}

#[test]
fn a_server_error_ends_the_session() {
    let mut session = registered_session();
    let outcome = feed(&mut session, "ERROR :Closing link");
    assert!(matches!(outcome.events[0], Event::Ended(_)));
}

// --- channels --------------------------------------------------------------

#[test]
fn joining_creates_a_channel_and_a_names_listing_fills_it() {
    let session = in_channel();
    let channel = session.channel(b"#test").expect("should have the channel");

    assert_eq!(channel.len(), 3);
    assert_eq!(
        channel
            .member(session.isupport(), b"alice")
            .unwrap()
            .prefixes,
        b"@"
    );
    assert_eq!(
        channel
            .member(session.isupport(), b"carol")
            .unwrap()
            .prefixes,
        b"+"
    );
    assert!(
        channel
            .member(session.isupport(), b"bob")
            .unwrap()
            .prefixes
            .is_empty()
    );
}

#[test]
fn channels_are_matched_by_the_networks_casemapping() {
    let session = in_channel();
    assert!(session.channel(b"#TEST").is_some());
}

#[test]
fn someone_else_joining_is_added_to_the_roster() {
    let mut session = in_channel();
    let outcome = feed(&mut session, ":dave!u@h JOIN #test");

    assert_eq!(session.channel(b"#test").unwrap().len(), 4);
    assert!(matches!(
        outcome.events[0],
        Event::Joined { is_self: false, .. }
    ));
}

#[test]
fn extended_join_records_the_account() {
    let mut session = in_channel();
    feed(
        &mut session,
        ":dave!u@h JOIN #test daveaccount :Dave Example",
    );

    let channel = session.channel(b"#test").unwrap();
    let dave = channel.member(session.isupport(), b"dave").unwrap();
    assert_eq!(dave.account.as_deref(), Some(&b"daveaccount"[..]));
}

#[test]
fn an_extended_join_with_no_account_records_none() {
    let mut session = in_channel();
    feed(&mut session, ":dave!u@h JOIN #test * :Dave Example");

    let channel = session.channel(b"#test").unwrap();
    assert!(
        channel
            .member(session.isupport(), b"dave")
            .unwrap()
            .account
            .is_none()
    );
}

#[test]
fn our_own_part_drops_the_channel_entirely() {
    let mut session = in_channel();
    let outcome = feed(&mut session, ":alice!u@h PART #test :bye");

    assert!(session.channel(b"#test").is_none());
    assert!(matches!(
        outcome.events[0],
        Event::Parted { is_self: true, .. }
    ));
}

#[test]
fn someone_else_parting_only_leaves_the_roster() {
    let mut session = in_channel();
    feed(&mut session, ":bob!u@h PART #test");

    let channel = session.channel(b"#test").expect("we are still in it");
    assert_eq!(channel.len(), 2);
    assert!(!channel.contains(session.isupport(), b"bob"));
}

#[test]
fn a_quit_removes_someone_from_every_shared_channel() {
    let mut session = in_channel();
    feed(&mut session, ":alice!u@h JOIN #other");
    feed(&mut session, ":srv 353 alice = #other :@alice bob");
    feed(&mut session, ":srv 366 alice #other :End");

    let outcome = feed(&mut session, ":bob!u@h QUIT :gone");

    assert!(
        !session
            .channel(b"#test")
            .unwrap()
            .contains(session.isupport(), b"bob")
    );
    assert!(
        !session
            .channel(b"#other")
            .unwrap()
            .contains(session.isupport(), b"bob")
    );

    let Event::Quit { channels, .. } = outcome
        .events
        .iter()
        .find(|e| matches!(e, Event::Quit { .. }))
        .expect("should report a quit")
    else {
        unreachable!()
    };
    assert_eq!(channels.len(), 2, "both channels should be named");
}

#[test]
fn being_kicked_drops_the_channel() {
    let mut session = in_channel();
    let outcome = feed(&mut session, ":bob!u@h KICK #test alice :out");

    assert!(session.channel(b"#test").is_none());
    assert!(matches!(
        outcome.events[0],
        Event::Kicked { is_self: true, .. }
    ));
}

#[test]
fn a_nick_change_follows_someone_through_their_channels() {
    let mut session = in_channel();
    feed(&mut session, ":bob!u@h NICK bobby");

    let channel = session.channel(b"#test").unwrap();
    assert!(!channel.contains(session.isupport(), b"bob"));
    assert!(channel.contains(session.isupport(), b"bobby"));
}

#[test]
fn our_own_nick_change_updates_who_we_think_we_are() {
    let mut session = in_channel();
    feed(&mut session, ":alice!u@h NICK alice2");

    assert_eq!(session.nick(), b"alice2");
    assert!(session.is_me(b"alice2"));
    assert!(!session.is_me(b"alice"));
}

#[test]
fn a_nick_change_keeps_channel_status() {
    let mut session = in_channel();
    feed(&mut session, ":alice!u@h NICK alice2");

    let channel = session.channel(b"#test").unwrap();
    assert_eq!(
        channel
            .member(session.isupport(), b"alice2")
            .unwrap()
            .prefixes,
        b"@",
        "ops should survive a rename"
    );
}

// --- modes -----------------------------------------------------------------

#[test]
fn granting_op_updates_the_roster() {
    let mut session = in_channel();
    feed(&mut session, ":alice!u@h MODE #test +o bob");

    let channel = session.channel(b"#test").unwrap();
    assert_eq!(
        channel.member(session.isupport(), b"bob").unwrap().prefixes,
        b"@"
    );
}

#[test]
fn parameterised_modes_pair_with_the_right_nickname() {
    // `+lo 20 bob` means limit 20 and op bob. Consuming the parameters in the
    // wrong order would hand ops to whoever happened to be named next.
    let mut session = in_channel();
    feed(&mut session, ":alice!u@h MODE #test +lo 20 bob");

    let channel = session.channel(b"#test").unwrap();
    assert_eq!(
        channel.member(session.isupport(), b"bob").unwrap().prefixes,
        b"@"
    );
}

#[test]
fn removing_a_limit_takes_no_parameter() {
    // `-l+o bob` has one parameter, and it belongs to the +o.
    let mut session = in_channel();
    feed(&mut session, ":alice!u@h MODE #test -l+o bob");

    let channel = session.channel(b"#test").unwrap();
    assert_eq!(
        channel.member(session.isupport(), b"bob").unwrap().prefixes,
        b"@"
    );
}

#[test]
fn revoking_op_updates_the_roster() {
    let mut session = in_channel();
    feed(&mut session, ":bob!u@h MODE #test -o alice");

    let channel = session.channel(b"#test").unwrap();
    assert!(
        channel
            .member(session.isupport(), b"alice")
            .unwrap()
            .prefixes
            .is_empty()
    );
}

// --- messages --------------------------------------------------------------

#[test]
fn a_channel_message_reports_its_target_and_sender() {
    let mut session = in_channel();
    let outcome = feed(&mut session, ":bob!u@h PRIVMSG #test :hello everyone");

    let Event::Message {
        target,
        from,
        text,
        kind,
        ..
    } = &outcome.events[0]
    else {
        panic!("expected a message, got {:?}", outcome.events[0]);
    };
    assert_eq!(*target, Target::Channel(b"#test".to_vec()));
    assert_eq!(from.nick, b"bob");
    assert_eq!(from.mask.as_deref(), Some(&b"bob!u@h"[..]));
    assert_eq!(text, b"hello everyone");
    assert_eq!(*kind, MessageKind::Privmsg);
}

#[test]
fn a_private_message_is_reported_as_direct() {
    let mut session = registered_session();
    let outcome = feed(&mut session, ":bob!u@h PRIVMSG alice :just you");

    let Event::Message { target, .. } = &outcome.events[0] else {
        panic!("expected a message");
    };
    assert_eq!(*target, Target::Direct(b"alice".to_vec()));
}

#[test]
fn notices_are_distinguished_from_messages() {
    let mut session = in_channel();
    let outcome = feed(&mut session, ":bob!u@h NOTICE #test :take note");

    let Event::Message { kind, .. } = &outcome.events[0] else {
        panic!("expected a message");
    };
    assert_eq!(*kind, MessageKind::Notice);
}

#[test]
fn server_time_and_account_tags_are_surfaced() {
    let mut session = in_channel();
    let outcome = feed(
        &mut session,
        "@time=2025-09-18T00:00:00.000Z;account=bobaccount :bob!u@h PRIVMSG #test :hi",
    );

    let Event::Message { time, from, .. } = &outcome.events[0] else {
        panic!("expected a message");
    };
    assert_eq!(time.as_deref(), Some("2025-09-18T00:00:00.000Z"));
    assert_eq!(from.account.as_deref(), Some(&b"bobaccount"[..]));
}

#[test]
fn a_tagmsg_surfaces_its_client_tags_unescaped() {
    let mut session = in_channel();
    let outcome = feed(&mut session, r"@+typing=a\sb :bob!u@h TAGMSG #test");

    let Event::TagMessage { target, tags, .. } = &outcome.events[0] else {
        panic!("expected a tag message, got {:?}", outcome.events[0]);
    };
    assert_eq!(*target, Target::Channel(b"#test".to_vec()));
    assert_eq!(tags[0].0, b"+typing");
    assert_eq!(tags[0].1, b"a b", "the value should be unescaped");
}

// --- topics and presence ---------------------------------------------------

#[test]
fn a_topic_reply_is_recorded_and_reported() {
    let mut session = in_channel();
    feed(&mut session, ":srv 332 alice #test :the topic");
    feed(&mut session, ":srv 333 alice #test bob 1700000000");

    let channel = session.channel(b"#test").unwrap();
    assert_eq!(channel.topic.as_deref(), Some(&b"the topic"[..]));
    assert_eq!(channel.topic_setter.as_deref(), Some(&b"bob"[..]));
}

#[test]
fn a_topic_change_updates_the_channel() {
    let mut session = in_channel();
    let outcome = feed(&mut session, ":bob!u@h TOPIC #test :a new topic");

    assert_eq!(
        session.channel(b"#test").unwrap().topic.as_deref(),
        Some(&b"a new topic"[..])
    );
    assert!(matches!(outcome.events[0], Event::Topic { .. }));
}

#[test]
fn away_notifications_update_the_roster() {
    let mut session = in_channel();
    feed(&mut session, ":bob!u@h AWAY :lunch");
    assert!(
        session
            .channel(b"#test")
            .unwrap()
            .member(session.isupport(), b"bob")
            .unwrap()
            .away
    );

    feed(&mut session, ":bob!u@h AWAY");
    assert!(
        !session
            .channel(b"#test")
            .unwrap()
            .member(session.isupport(), b"bob")
            .unwrap()
            .away
    );
}

#[test]
fn an_account_change_updates_the_roster() {
    let mut session = in_channel();
    feed(&mut session, ":bob!u@h ACCOUNT bobaccount");
    assert_eq!(
        session
            .channel(b"#test")
            .unwrap()
            .member(session.isupport(), b"bob")
            .unwrap()
            .account
            .as_deref(),
        Some(&b"bobaccount"[..])
    );

    feed(&mut session, ":bob!u@h ACCOUNT *");
    assert!(
        session
            .channel(b"#test")
            .unwrap()
            .member(session.isupport(), b"bob")
            .unwrap()
            .account
            .is_none(),
        "`*` means logged out"
    );
}

#[test]
fn an_invitation_is_reported() {
    let mut session = registered_session();
    let outcome = feed(&mut session, ":bob!u@h INVITE alice #secret");

    let Event::Invited { channel, by } = &outcome.events[0] else {
        panic!("expected an invitation");
    };
    assert_eq!(channel, b"#secret");
    assert_eq!(by.nick, b"bob");
}

#[test]
fn unrecognised_commands_are_passed_through_rather_than_dropped() {
    let mut session = registered_session();
    let outcome = feed(&mut session, ":srv SOMETHINGNEW arg :text");
    assert!(matches!(outcome.events[0], Event::Raw(_)));
}
