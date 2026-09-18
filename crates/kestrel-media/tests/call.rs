//! Two peer connections negotiating a real call between them.
//!
//! No camera, no microphone and no second machine: test patterns stand in for
//! capture, and both ends run in this process. What is real is everything
//! else — ICE, DTLS, SRTP and the RTP that carries the media.

use std::time::Duration;

use kestrel_media::{PeerConnection, PeerEvent, Sending, Source};
use tokio::sync::mpsc::UnboundedReceiver;

/// Which end of the negotiation a peer is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Offerer,
    Answerer,
}

/// Drive both ends until they connect, or give up.
///
/// Signalling here is a direct function call rather than a network hop; the
/// point is to prove the media path, not to re-test the transport.
async fn connect_pair(sending: Sending) -> (PeerConnection, PeerConnection) {
    kestrel_media::init().expect("GStreamer should initialise");

    let (alice, mut alice_events) =
        PeerConnection::new("alice", sending, Source::Test, None).expect("alice should build");
    let (bob, mut bob_events) =
        PeerConnection::new("bob", sending, Source::Test, None).expect("bob should build");

    let mut alice_connected = false;
    let mut bob_connected = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline && !(alice_connected && bob_connected) {
        tokio::select! {
            Some(event) = alice_events.recv() => {
                handle(&alice, &bob, event, Role::Offerer, &mut alice_connected);
            }
            Some(event) = bob_events.recv() => {
                handle(&bob, &alice, event, Role::Answerer, &mut bob_connected);
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    assert!(
        alice_connected && bob_connected,
        "the peers did not connect: alice={alice_connected} bob={bob_connected}"
    );
    (alice, bob)
}

fn handle(
    own: &PeerConnection,
    peer: &PeerConnection,
    event: PeerEvent,
    role: Role,
    connected: &mut bool,
) {
    match event {
        // Only the offerer opens negotiation; both sides reacting would
        // produce competing offers, which is the classic glare failure.
        PeerEvent::NegotiationNeeded => {
            if role == Role::Offerer {
                own.create_offer();
            }
        }

        PeerEvent::LocalDescription { kind, sdp } => {
            peer.set_remote_description(&kind, &sdp)
                .expect("a description we generated should parse");
            if kind == "offer" {
                peer.create_answer();
            }
        }
        PeerEvent::IceCandidate {
            mline_index,
            candidate,
        } => peer.add_ice_candidate(mline_index, &candidate),

        PeerEvent::ConnectionState(state) => {
            assert!(
                !state.is_terminal(),
                "the connection ended in {state:?} rather than connecting"
            );
            *connected = state.is_connected();
        }
        PeerEvent::Error(error) => panic!("pipeline failed: {error}"),
        PeerEvent::IceState(_) | PeerEvent::RemoteTrack { .. } => {}
    }
}

/// Total bytes received across every inbound RTP stream.
fn bytes_received(peer: &PeerConnection) -> u64 {
    let Some(stats) = peer.stats() else {
        return 0;
    };
    let mut total = 0;
    // The stats structure nests one structure per stream; the inbound ones
    // carry `bytes-received`.
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
async fn two_peers_negotiate_and_exchange_media() {
    let (alice, bob) = connect_pair(Sending::audio_video()).await;

    // Connected is not the same as carrying anything; give the streams a
    // moment and then insist that bytes actually moved.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let to_alice = bytes_received(&alice);
    let to_bob = bytes_received(&bob);
    assert!(to_alice > 0, "alice received nothing");
    assert!(to_bob > 0, "bob received nothing");

    alice.close();
    bob.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_audio_only_call_connects() {
    let (alice, bob) = connect_pair(Sending::audio_only()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(bytes_received(&bob) > 0, "bob received no audio");

    alice.close();
    bob.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_track_is_reported_before_media_arrives() {
    // A client needs to know a stream exists in order to attach a sink to it.
    kestrel_media::init().expect("GStreamer should initialise");

    let (alice, mut alice_events) =
        PeerConnection::new("alice", Sending::audio_only(), Source::Test, None).unwrap();
    let (bob, mut bob_events) =
        PeerConnection::new("bob", Sending::audio_only(), Source::Test, None).unwrap();

    let mut saw_track = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    while tokio::time::Instant::now() < deadline && !saw_track {
        tokio::select! {
            Some(event) = alice_events.recv() => {
                if matches!(event, PeerEvent::RemoteTrack { .. }) {
                    saw_track = true;
                }
                handle(&alice, &bob, event, Role::Offerer, &mut false);
            }
            Some(event) = bob_events.recv() => {
                if matches!(event, PeerEvent::RemoteTrack { .. }) {
                    saw_track = true;
                }
                handle(&bob, &alice, event, Role::Answerer, &mut false);
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    assert!(saw_track, "no remote track was ever reported");
    alice.close();
    bob.close();
}

#[test]
fn a_connection_releases_its_devices_when_dropped() {
    // Otherwise the camera light stays on after a call ends, which users
    // notice immediately and rightly distrust.
    kestrel_media::init().expect("GStreamer should initialise");
    let (peer, _events) =
        PeerConnection::new("dropped", Sending::audio_only(), Source::Test, None).unwrap();
    drop(peer);
}

/// Drain an event receiver, for tests that do not care what is in it.
#[allow(dead_code)]
fn drain(receiver: &mut UnboundedReceiver<PeerEvent>) {
    while receiver.try_recv().is_ok() {}
}
