//! Shared helpers for the server integration tests.
//!
//! Assertions are made against the *rendered* wire form rather than against
//! internal structures. That way each test exercises the serialiser too, and a
//! failure reports the bytes a real client would have received.

#![allow(dead_code)]

use kestrel_proto::Message;
use kestreld_core::{Action, ClientId, Server, ServerConfig};

/// A server with predictable settings for tests.
#[must_use]
pub fn test_server() -> Server {
    Server::new(
        ServerConfig {
            server_name: b"test.server".to_vec(),
            network_name: b"TestNet".to_vec(),
            version: b"kestreld-test".to_vec(),
            ..ServerConfig::default()
        },
        1_758_153_600, // 2025-09-18 00:00:00 UTC
    )
}

/// Feed one line to the server as `id`, returning the actions it produced.
pub fn feed(server: &mut Server, id: ClientId, line: &str) -> Vec<Action> {
    feed_at(server, id, line, 1_758_153_600)
}

/// Feed one line at a specific timestamp.
pub fn feed_at(server: &mut Server, id: ClientId, line: &str, now: u64) -> Vec<Action> {
    let bytes = line.as_bytes().to_vec();
    let msg = Message::parse(&bytes).expect("test line should parse");
    server.handle_to_vec(id, &msg, now)
}

/// Connect a client and take it all the way through registration.
pub fn register(server: &mut Server, nick: &str) -> ClientId {
    let id = server.connect(b"example.host".to_vec());
    feed(server, id, &format!("NICK {nick}"));
    feed(server, id, &format!("USER {nick} 0 * :{nick} the tester"));
    assert!(
        server
            .client(id)
            .expect("client should exist")
            .is_registered(),
        "{nick} should be registered"
    );
    id
}

/// Every line delivered to `to`, rendered as text with the CRLF stripped.
#[must_use]
pub fn lines_to(actions: &[Action], to: ClientId) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Send {
                to: target,
                message,
            } if *target == to => {
                let bytes = message.to_vec().expect("action should serialise");
                Some(
                    String::from_utf8_lossy(&bytes)
                        .trim_end_matches("\r\n")
                        .to_string(),
                )
            }
            _ => None,
        })
        .collect()
}

/// Every line delivered to anyone.
#[must_use]
pub fn all_lines(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Send { message, .. } => {
                let bytes = message.to_vec().expect("action should serialise");
                Some(
                    String::from_utf8_lossy(&bytes)
                        .trim_end_matches("\r\n")
                        .to_string(),
                )
            }
            Action::Close { .. } => None,
        })
        .collect()
}

/// The numeric reply codes sent to `to`, in order.
#[must_use]
pub fn numerics_to(actions: &[Action], to: ClientId) -> Vec<u16> {
    lines_to(actions, to)
        .iter()
        .filter_map(|line| Message::parse(line.as_bytes()).ok()?.numeric())
        .collect()
}

/// Whether any line sent to `to` contains `needle`.
#[must_use]
pub fn any_line_to(actions: &[Action], to: ClientId, needle: &str) -> bool {
    lines_to(actions, to).iter().any(|l| l.contains(needle))
}

/// Assert that `to` received a line containing `needle`.
pub fn assert_received(actions: &[Action], to: ClientId, needle: &str) {
    let lines = lines_to(actions, to);
    assert!(
        lines.iter().any(|l| l.contains(needle)),
        "expected a line containing {needle:?}, got {lines:#?}"
    );
}

/// Assert that `to` received no line containing `needle`.
pub fn assert_not_received(actions: &[Action], to: ClientId, needle: &str) {
    let lines = lines_to(actions, to);
    assert!(
        !lines.iter().any(|l| l.contains(needle)),
        "did not expect a line containing {needle:?}, got {lines:#?}"
    );
}

/// Assert that `to` received nothing at all.
pub fn assert_silent(actions: &[Action], to: ClientId) {
    let lines = lines_to(actions, to);
    assert!(lines.is_empty(), "expected silence, got {lines:#?}");
}
