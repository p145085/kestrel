//! Capability negotiation and SASL authentication end to end.

mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use common::{assert_received, feed, numerics_to, test_server};
use kestrel_proto::numeric;
use kestreld_core::{ClientId, Server};

/// A server with one registered account, plus a connected, unregistered client
/// that has already enabled the `sasl` capability.
fn server_with_account() -> (Server, ClientId) {
    let mut server = test_server();
    server
        .accounts_mut()
        .register(b"alice", b"hunter2", 0)
        .expect("account should register");

    let id = server.connect(b"example.host".to_vec());
    feed(&mut server, id, "CAP LS 302");
    feed(&mut server, id, "CAP REQ :sasl");
    (server, id)
}

#[allow(clippy::similar_names)]
fn plain(authzid: &str, authcid: &str, password: &str) -> String {
    BASE64.encode(format!("{authzid}\0{authcid}\0{password}"))
}

#[test]
fn cap_ls_advertises_sasl_with_its_mechanisms() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    let actions = feed(&mut server, id, "CAP LS 302");
    assert_received(&actions, id, "CAP * LS :sasl=PLAIN,EXTERNAL");
}

#[test]
fn requesting_sasl_is_acknowledged() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    let actions = feed(&mut server, id, "CAP REQ :sasl");

    assert_received(&actions, id, "CAP * ACK :sasl");
    assert!(server.client(id).unwrap().has_cap("sasl"));
}

#[test]
fn a_request_is_all_or_nothing() {
    // Enabling only the recognised half would leave client and server
    // disagreeing about which capabilities are active.
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    let actions = feed(&mut server, id, "CAP REQ :sasl not-a-real-cap");

    assert_received(&actions, id, "CAP * NAK :sasl not-a-real-cap");
    assert!(
        !server.client(id).unwrap().has_cap("sasl"),
        "no capability should be enabled when the request is refused"
    );
}

#[test]
fn a_capability_can_be_disabled_again() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    feed(&mut server, id, "CAP REQ :sasl");
    let actions = feed(&mut server, id, "CAP REQ :-sasl");

    assert_received(&actions, id, "CAP * ACK :-sasl");
    assert!(!server.client(id).unwrap().has_cap("sasl"));
}

#[test]
fn cap_list_reports_what_is_enabled() {
    let mut server = test_server();
    let id = server.connect(b"example.host".to_vec());
    feed(&mut server, id, "CAP REQ :sasl");

    let actions = feed(&mut server, id, "CAP LIST");
    assert_received(&actions, id, "CAP * LIST :sasl");
}

// --- PLAIN -----------------------------------------------------------------

#[test]
fn a_correct_plain_exchange_logs_the_client_in() {
    let (mut server, id) = server_with_account();

    let challenge = feed(&mut server, id, "AUTHENTICATE PLAIN");
    assert_received(&challenge, id, "AUTHENTICATE +");

    let actions = feed(
        &mut server,
        id,
        &format!("AUTHENTICATE {}", plain("", "alice", "hunter2")),
    );

    assert_received(&actions, id, "You are now logged in as alice");
    assert_eq!(
        numerics_to(&actions, id),
        vec![numeric::RPL_LOGGEDIN, numeric::RPL_SASLSUCCESS]
    );
    assert_eq!(server.client(id).unwrap().account(), Some(&b"alice"[..]));
}

#[test]
fn a_wrong_password_fails() {
    let (mut server, id) = server_with_account();
    feed(&mut server, id, "AUTHENTICATE PLAIN");

    let actions = feed(
        &mut server,
        id,
        &format!("AUTHENTICATE {}", plain("", "alice", "wrong")),
    );

    assert_eq!(numerics_to(&actions, id), vec![numeric::ERR_SASLFAIL]);
    assert!(server.client(id).unwrap().account().is_none());
}

#[test]
fn an_unknown_account_fails_identically_to_a_wrong_password() {
    // The two must be indistinguishable, or SASL becomes an account
    // enumeration oracle.
    let (mut server, id) = server_with_account();

    feed(&mut server, id, "AUTHENTICATE PLAIN");
    let unknown = feed(
        &mut server,
        id,
        &format!("AUTHENTICATE {}", plain("", "nobody", "hunter2")),
    );

    feed(&mut server, id, "AUTHENTICATE PLAIN");
    let wrong = feed(
        &mut server,
        id,
        &format!("AUTHENTICATE {}", plain("", "alice", "wrong")),
    );

    assert_eq!(
        common::lines_to(&unknown, id),
        common::lines_to(&wrong, id),
        "the two failures must be byte-identical"
    );
}

#[test]
fn authenticating_as_another_account_is_refused() {
    let (mut server, id) = server_with_account();
    feed(&mut server, id, "AUTHENTICATE PLAIN");

    let actions = feed(
        &mut server,
        id,
        &format!("AUTHENTICATE {}", plain("someone-else", "alice", "hunter2")),
    );
    assert_eq!(numerics_to(&actions, id), vec![numeric::ERR_SASLFAIL]);
    assert!(server.client(id).unwrap().account().is_none());
}

#[test]
fn an_authzid_matching_the_account_is_accepted() {
    let (mut server, id) = server_with_account();
    feed(&mut server, id, "AUTHENTICATE PLAIN");

    let actions = feed(
        &mut server,
        id,
        &format!("AUTHENTICATE {}", plain("alice", "alice", "hunter2")),
    );
    assert!(numerics_to(&actions, id).contains(&numeric::RPL_SASLSUCCESS));
}

#[test]
fn authenticate_without_the_capability_is_refused() {
    let mut server = test_server();
    server
        .accounts_mut()
        .register(b"alice", b"hunter2", 0)
        .unwrap();
    let id = server.connect(b"example.host".to_vec());

    let actions = feed(&mut server, id, "AUTHENTICATE PLAIN");
    assert_eq!(numerics_to(&actions, id), vec![numeric::ERR_SASLFAIL]);
}

#[test]
fn a_client_can_abort_an_exchange() {
    let (mut server, id) = server_with_account();
    feed(&mut server, id, "AUTHENTICATE PLAIN");

    let actions = feed(&mut server, id, "AUTHENTICATE *");
    assert_eq!(numerics_to(&actions, id), vec![numeric::ERR_SASLABORTED]);
}

#[test]
fn authenticating_twice_is_refused() {
    let (mut server, id) = server_with_account();
    feed(&mut server, id, "AUTHENTICATE PLAIN");
    feed(
        &mut server,
        id,
        &format!("AUTHENTICATE {}", plain("", "alice", "hunter2")),
    );

    let actions = feed(&mut server, id, "AUTHENTICATE PLAIN");
    assert_eq!(numerics_to(&actions, id), vec![numeric::ERR_SASLALREADY]);
}

#[test]
fn an_unsupported_mechanism_fails() {
    let (mut server, id) = server_with_account();
    let actions = feed(&mut server, id, "AUTHENTICATE SCRAM-SHA-256");
    assert_eq!(numerics_to(&actions, id), vec![numeric::ERR_SASLFAIL]);
}

#[test]
fn a_malformed_payload_fails_without_panicking() {
    let (mut server, id) = server_with_account();
    for bad in ["!!!!", "AAAA", "+"] {
        feed(&mut server, id, "AUTHENTICATE PLAIN");
        let actions = feed(&mut server, id, &format!("AUTHENTICATE {bad}"));
        assert_eq!(
            numerics_to(&actions, id),
            vec![numeric::ERR_SASLFAIL],
            "{bad:?} should fail cleanly"
        );
    }
}

// --- EXTERNAL --------------------------------------------------------------

#[test]
fn external_authenticates_by_client_certificate() {
    let (mut server, id) = server_with_account();
    server.accounts_mut().add_certificate(b"alice", "aabbcc");
    server.set_certificate_fingerprint(id, "AABBCC".to_owned());

    feed(&mut server, id, "AUTHENTICATE EXTERNAL");
    let actions = feed(&mut server, id, "AUTHENTICATE +");

    assert!(numerics_to(&actions, id).contains(&numeric::RPL_SASLSUCCESS));
    assert_eq!(server.client(id).unwrap().account(), Some(&b"alice"[..]));
}

#[test]
fn external_fails_without_a_certificate() {
    let (mut server, id) = server_with_account();
    server.accounts_mut().add_certificate(b"alice", "aabbcc");
    // No fingerprint was supplied by the transport.

    feed(&mut server, id, "AUTHENTICATE EXTERNAL");
    let actions = feed(&mut server, id, "AUTHENTICATE +");
    assert_eq!(numerics_to(&actions, id), vec![numeric::ERR_SASLFAIL]);
}

#[test]
fn external_fails_for_an_unrecognised_certificate() {
    let (mut server, id) = server_with_account();
    server.set_certificate_fingerprint(id, "ffffff".to_owned());

    feed(&mut server, id, "AUTHENTICATE EXTERNAL");
    let actions = feed(&mut server, id, "AUTHENTICATE +");
    assert_eq!(numerics_to(&actions, id), vec![numeric::ERR_SASLFAIL]);
}

// --- interaction with registration -----------------------------------------

#[test]
fn authentication_survives_into_the_registered_session() {
    let (mut server, id) = server_with_account();
    feed(&mut server, id, "AUTHENTICATE PLAIN");
    feed(
        &mut server,
        id,
        &format!("AUTHENTICATE {}", plain("", "alice", "hunter2")),
    );

    feed(&mut server, id, "NICK alice");
    feed(&mut server, id, "USER alice 0 * :Alice");
    let actions = feed(&mut server, id, "CAP END");

    assert!(numerics_to(&actions, id).contains(&numeric::RPL_WELCOME));
    assert_eq!(server.client(id).unwrap().account(), Some(&b"alice"[..]));
    assert!(server.client(id).unwrap().is_authenticated());
}
