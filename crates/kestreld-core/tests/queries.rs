//! `WHO`, `LIST`, `ISON` and `USERHOST`, and what they disclose.

mod common;

use common::{
    assert_not_received, assert_received, feed, lines_to, numerics_to, register, test_server,
};
use kestrel_proto::numeric;

#[test]
fn who_on_a_channel_lists_its_members() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");

    let actions = feed(&mut server, alice, "WHO #chan");

    assert_received(
        &actions,
        alice,
        "#chan ~alice example.host test.server alice H@ :0 alice the tester",
    );
    assert_received(
        &actions,
        alice,
        "#chan ~bob example.host test.server bob H :0 bob the tester",
    );
    assert!(numerics_to(&actions, alice).contains(&numeric::RPL_ENDOFWHO));
}

#[test]
fn who_marks_away_users_as_gone() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #chan");
    feed(&mut server, bob, "JOIN #chan");
    feed(&mut server, bob, "AWAY :lunch");

    let actions = feed(&mut server, alice, "WHO #chan");
    assert_received(&actions, alice, "bob G :0 bob");
}

#[test]
fn who_accepts_a_nickname() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    register(&mut server, "bob");

    let actions = feed(&mut server, alice, "WHO bob");
    assert_received(&actions, alice, "bob H :0 bob the tester");
}

#[test]
fn who_accepts_a_host_mask() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    register(&mut server, "bob");

    let actions = feed(&mut server, alice, "WHO *!*@example.host");
    assert_received(&actions, alice, " alice ");
    assert_received(&actions, alice, " bob ");
}

#[test]
fn who_always_terminates_even_with_no_matches() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    let actions = feed(&mut server, alice, "WHO #nonexistent");
    assert_eq!(numerics_to(&actions, alice), vec![numeric::RPL_ENDOFWHO]);
}

#[test]
fn a_secret_channel_is_not_disclosed_by_who() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let outsider = register(&mut server, "outsider");
    feed(&mut server, alice, "JOIN #secret");
    feed(&mut server, alice, "MODE #secret +s");

    // Naming the channel directly reveals nothing.
    let direct = feed(&mut server, outsider, "WHO #secret");
    assert_eq!(numerics_to(&direct, outsider), vec![numeric::RPL_ENDOFWHO]);

    // Nor does asking about a member of it.
    let indirect = feed(&mut server, outsider, "WHO alice");
    assert_not_received(&indirect, outsider, "#secret");

    // A member still sees it.
    let member = feed(&mut server, alice, "WHO #secret");
    assert_received(&member, alice, "#secret");
}

// --- LIST ------------------------------------------------------------------

#[test]
fn list_reports_every_visible_channel_with_counts_and_topics() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, alice, "JOIN #one");
    feed(&mut server, alice, "TOPIC #one :the first channel");
    feed(&mut server, bob, "JOIN #one");
    feed(&mut server, bob, "JOIN #two");

    let actions = feed(&mut server, alice, "LIST");

    assert!(numerics_to(&actions, alice).contains(&numeric::RPL_LISTSTART));
    assert_received(&actions, alice, "#one 2 :the first channel");
    assert_received(&actions, alice, "#two 1 :");
    assert!(numerics_to(&actions, alice).contains(&numeric::RPL_LISTEND));
}

#[test]
fn list_can_be_narrowed_to_named_channels() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    feed(&mut server, alice, "JOIN #one,#two,#three");

    let actions = feed(&mut server, alice, "LIST #two");
    assert_received(&actions, alice, "#two");
    assert_not_received(&actions, alice, "#one");
    assert_not_received(&actions, alice, "#three");
}

#[test]
fn secret_channels_are_omitted_from_list() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let outsider = register(&mut server, "outsider");
    feed(&mut server, alice, "JOIN #secret");
    feed(&mut server, alice, "MODE #secret +s");

    let hidden = feed(&mut server, outsider, "LIST");
    assert_not_received(&hidden, outsider, "#secret");

    let shown = feed(&mut server, alice, "LIST");
    assert_received(&shown, alice, "#secret");
}

// --- ISON and USERHOST -----------------------------------------------------

#[test]
fn ison_reports_only_the_nicknames_that_are_online() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    register(&mut server, "bob");

    let actions = feed(&mut server, alice, "ISON alice bob nobody");
    let lines = lines_to(&actions, alice);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("alice"), "got {lines:?}");
    assert!(lines[0].contains("bob"), "got {lines:?}");
    assert!(!lines[0].contains("nobody"), "got {lines:?}");
}

#[test]
fn ison_accepts_nicknames_in_a_trailing_parameter() {
    // Clients send both forms; accepting only one of them looks like a bug
    // that affects some clients and not others.
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    register(&mut server, "bob");

    let actions = feed(&mut server, alice, "ISON :alice bob");
    assert_received(&actions, alice, "bob");
}

#[test]
fn ison_reports_the_canonical_case_of_a_nickname() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    register(&mut server, "Bob");

    let actions = feed(&mut server, alice, "ISON BOB");
    assert_received(&actions, alice, "Bob");
}

#[test]
fn userhost_reports_masks_and_away_state() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let bob = register(&mut server, "bob");
    feed(&mut server, bob, "AWAY :out");

    let actions = feed(&mut server, alice, "USERHOST alice bob");
    // `+` means here, `-` means away.
    assert_received(&actions, alice, "alice=+~alice@example.host");
    assert_received(&actions, alice, "bob=-~bob@example.host");
}

#[test]
fn userhost_is_capped_so_the_reply_fits_one_line() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    for nick in ["b", "c", "d", "e", "f", "g"] {
        register(&mut server, nick);
    }

    let actions = feed(&mut server, alice, "USERHOST alice b c d e f g");
    let lines = lines_to(&actions, alice);
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].matches('=').count(),
        5,
        "at most five replies, got {lines:?}"
    );
}
