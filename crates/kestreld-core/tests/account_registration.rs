//! `REGISTER` — creating an account from a client.

mod common;

use common::{assert_received, feed, numerics_to, register, test_server};
use kestrel_proto::numeric;
use kestreld_core::{ClientId, Server};

/// Connect a client that has enabled the registration capability but has not
/// finished registering with the server.
fn pre_connection_client(server: &mut Server, nick: &str) -> ClientId {
    let id = server.connect(b"example.host".to_vec());
    feed(server, id, "CAP LS 302");
    feed(server, id, "CAP REQ :draft/account-registration");
    feed(server, id, &format!("NICK {nick}"));
    id
}

#[test]
fn the_capability_is_advertised_with_its_policy() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    let actions = feed(&mut server, id, "CAP LS 302");
    assert_received(&actions, id, "draft/account-registration=before-connect");
}

#[test]
fn registering_creates_the_account_and_logs_the_client_in() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    let actions = feed(&mut server, alice, "REGISTER alice * hunter2hunter2");

    assert_received(&actions, alice, "REGISTER SUCCESS alice :Account created");
    assert!(numerics_to(&actions, alice).contains(&numeric::RPL_LOGGEDIN));
    assert_eq!(server.client(alice).unwrap().account(), Some(&b"alice"[..]));
    assert!(server.accounts().get(b"alice").is_some());
}

#[test]
fn a_star_account_name_means_use_my_nickname() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    let actions = feed(&mut server, alice, "REGISTER * * hunter2hunter2");
    assert_received(&actions, alice, "REGISTER SUCCESS alice");
}

#[test]
fn registration_works_before_the_connection_completes() {
    // The capability is advertised as `before-connect`, so this has to work
    // during capability negotiation, not only afterwards.
    let mut server = test_server();
    let alice = pre_connection_client(&mut server, "alice");

    let actions = feed(&mut server, alice, "REGISTER * * hunter2hunter2");
    assert_received(&actions, alice, "REGISTER SUCCESS alice");
    assert!(!server.client(alice).unwrap().is_registered());
    assert_eq!(server.client(alice).unwrap().account(), Some(&b"alice"[..]));
}

#[test]
fn the_new_account_can_be_used_to_authenticate() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    feed(&mut server, alice, "REGISTER alice * hunter2hunter2");

    // A second client authenticates with the credentials just created.
    let bob = server.connect(b"example.host".to_vec());
    feed(&mut server, bob, "CAP REQ :sasl");
    feed(&mut server, bob, "AUTHENTICATE PLAIN");
    // "\0alice\0hunter2hunter2", base64-encoded.
    let actions = feed(
        &mut server,
        bob,
        "AUTHENTICATE AGFsaWNlAGh1bnRlcjJodW50ZXIy",
    );

    assert!(numerics_to(&actions, bob).contains(&numeric::RPL_SASLSUCCESS));
}

#[test]
fn a_taken_account_name_is_refused() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    feed(&mut server, alice, "REGISTER alice * hunter2hunter2");

    let bob = register(&mut server, "bob");
    let actions = feed(&mut server, bob, "REGISTER alice * differentpassword");
    assert_received(&actions, bob, "FAIL REGISTER ACCOUNT_EXISTS");
}

#[test]
fn a_short_password_is_refused() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let actions = feed(&mut server, alice, "REGISTER alice * short");

    assert_received(&actions, alice, "FAIL REGISTER UNACCEPTABLE_PASSWORD");
    assert!(server.accounts().get(b"alice").is_none());
}

#[test]
fn an_invalid_account_name_is_refused() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");

    // A name containing a space cannot be tested here: it would be two
    // parameters on the wire, so no client can send one in the first place.
    for bad in ["#channel", "-leading", "with@at", "_leading"] {
        let actions = feed(
            &mut server,
            alice,
            &format!("REGISTER {bad} * hunter2hunter2"),
        );
        assert_received(&actions, alice, "FAIL REGISTER INVALID_ACCOUNT_NAME");
    }
}

#[test]
fn missing_parameters_are_refused() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    let actions = feed(&mut server, alice, "REGISTER alice");
    assert_received(&actions, alice, "FAIL REGISTER NEED_MORE_PARAMS");
}

#[test]
fn an_already_authenticated_client_cannot_register_again() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    feed(&mut server, alice, "REGISTER alice * hunter2hunter2");

    let actions = feed(&mut server, alice, "REGISTER other * hunter2hunter2");
    assert_received(&actions, alice, "FAIL REGISTER ALREADY_AUTHENTICATED");
}

#[test]
fn registration_marks_the_store_as_needing_saving() {
    // Without this the account exists only in memory and is lost on restart.
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    assert!(!server.take_accounts_changed());

    feed(&mut server, alice, "REGISTER alice * hunter2hunter2");
    assert!(server.take_accounts_changed());
    assert!(
        !server.take_accounts_changed(),
        "the flag should clear once taken"
    );
}

#[test]
fn a_failed_registration_does_not_mark_the_store_dirty() {
    let mut server = test_server();
    let alice = register(&mut server, "alice");
    server.take_accounts_changed();

    feed(&mut server, alice, "REGISTER alice * short");
    assert!(!server.take_accounts_changed());
}
