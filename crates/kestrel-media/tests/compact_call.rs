//! A real call whose descriptions travel through the compact form.
//!
//! `call.rs` proves two peers can talk when they hand each other raw SDP.
//! This proves they can still talk when everything goes through the few
//! hundred bytes that actually cross an IRC network — which is the only path
//! a real call ever takes.

use std::time::Duration;

use kestrel_media::{PeerConnection, PeerEvent, Sending, Source};
use kestrel_rtc_proto::{Candidate, sdp};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Offerer,
    Answerer,
}

/// Reduce SDP to the compact form and rebuild it, as the wire would.
fn through_compact(text: &str) -> String {
    let compact = sdp::from_sdp(text)
        .unwrap_or_else(|| panic!("could not reduce a description:\n{text}"));
    sdp::to_sdp(&compact)
}

/// Reduce a candidate and rebuild it, as the wire would.
fn candidate_through_compact(line: &str, mline_index: u32) -> Option<String> {
    let mid = if mline_index == 0 { "0" } else { "1" };
    Candidate::parse_sdp(line, mid).map(|c| c.to_sdp())
}

fn handle(
    own: &PeerConnection,
    peer: &PeerConnection,
    event: PeerEvent,
    role: Role,
    connected: &mut bool,
    errors: &mut Vec<String>,
    tracks: &mut Vec<String>,
) {
    match event {
        PeerEvent::NegotiationNeeded => {
            if role == Role::Offerer {
                own.create_offer();
            }
        }
        PeerEvent::LocalDescription { kind, sdp: text } => {
            let rebuilt = through_compact(&text);
            if let Err(error) = peer.set_remote_description(&kind, &rebuilt) {
                errors.push(format!("{kind} was refused: {error}"));
                return;
            }
            if kind == "offer" {
                peer.create_answer();
            }
        }
        PeerEvent::IceCandidate {
            mline_index,
            candidate,
        } => {
            if let Some(rebuilt) = candidate_through_compact(&candidate, mline_index) {
                peer.add_ice_candidate(mline_index, &rebuilt);
            }
        }
        PeerEvent::ConnectionState(state) => *connected = state.is_connected(),
        PeerEvent::Error(error) => errors.push(error),
        PeerEvent::RemoteTrack { kind } => tracks.push(kind),
        PeerEvent::IceState(_) => {}
    }
}

fn bytes_received(peer: &PeerConnection) -> u64 {
    let Some(stats) = peer.stats() else {
        return 0;
    };
    let mut total = 0;
    stats.iter().for_each(|(_, value)| {
        if let Ok(inner) = value.get::<gstreamer::Structure>()
            && let Ok(bytes) = inner.get::<u64>("bytes-received")
        {
            total += bytes;
        }
    });
    total
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_survives_the_compact_form() {
    kestrel_media::init().expect("GStreamer should initialise");
    let sending = Sending::audio_video();

    let (alice, mut alice_events) =
        PeerConnection::new("alice", sending, &Source::Test, None).unwrap();
    let (bob, mut bob_events) = PeerConnection::new("bob", sending, &Source::Test, None).unwrap();

    let mut alice_connected = false;
    let mut bob_connected = false;
    let mut errors = Vec::new();
    let mut alice_tracks = Vec::new();
    let mut bob_tracks = Vec::new();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline
        && !(alice_connected && bob_connected)
        && errors.is_empty()
    {
        tokio::select! {
            Some(event) = alice_events.recv() => {
                handle(&alice, &bob, event, Role::Offerer, &mut alice_connected, &mut errors, &mut alice_tracks);
            }
            Some(event) = bob_events.recv() => {
                handle(&bob, &alice, event, Role::Answerer, &mut bob_connected, &mut errors, &mut bob_tracks);
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    assert!(errors.is_empty(), "the pipeline reported: {errors:#?}");
    assert!(
        alice_connected && bob_connected,
        "did not connect: alice={alice_connected} bob={bob_connected}"
    );

    // Connected is not carrying. Anything that breaks the media path shows up
    // here rather than in the handshake, so keep listening rather than
    // sleeping: a receive chain that cannot decode fails seconds after the
    // handshake succeeds, and a test that stops reading never learns of it.
    let settle = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < settle {
        tokio::select! {
            Some(event) = alice_events.recv() => {
                handle(&alice, &bob, event, Role::Offerer, &mut alice_connected, &mut errors, &mut alice_tracks);
            }
            Some(event) = bob_events.recv() => {
                handle(&bob, &alice, event, Role::Answerer, &mut bob_connected, &mut errors, &mut bob_tracks);
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    assert!(errors.is_empty(), "the pipeline reported: {errors:#?}");

    let to_alice = bytes_received(&alice);
    let to_bob = bytes_received(&bob);

    assert!(to_alice > 0, "alice received nothing through the compact form");
    assert!(to_bob > 0, "bob received nothing through the compact form");

    // Bytes arriving proves the transport. Decoding them proves the receive
    // chain, which is a separate thing and the one that actually broke.
    for (who, mut tracks) in [("alice", alice_tracks), ("bob", bob_tracks)] {
        tracks.sort();
        assert_eq!(
            tracks,
            ["audio", "video"],
            "{who} did not decode both streams"
        );
    }

    alice.close();
    bob.close();
}
