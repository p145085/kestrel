//! Incoming video reaches a sink the caller supplied.
//!
//! The interface owns the widget that draws a call, and hands the engine a
//! plain element to feed. Both orderings have to work: the sink offered before
//! anything is decoded, which is the ordinary case, and offered afterwards,
//! which is what happens when a call connects faster than somebody can be
//! asked where to put it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gstreamer::prelude::*;
use kestrel_media::{PeerConnection, PeerEvent, Sending, Source};
use tokio::sync::mpsc::UnboundedReceiver;

/// Which end of the negotiation a peer is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Offerer,
    Answerer,
}

/// A sink that counts what it is given.
///
/// Stands in for the paintable sink the window would supply: what matters here
/// is that frames arrive at an element the caller chose, not what it does with
/// them.
fn counting_sink() -> (gstreamer::Element, Arc<AtomicUsize>) {
    let sink = gstreamer::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .expect("fakesink should exist");

    let frames = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&frames);
    if let Some(pad) = sink.static_pad("sink") {
        pad.add_probe(gstreamer::PadProbeType::BUFFER, move |_, _| {
            counted.fetch_add(1, Ordering::Relaxed);
            gstreamer::PadProbeReturn::Ok
        });
    }
    (sink, frames)
}

fn handle(
    own: &PeerConnection,
    peer: &PeerConnection,
    event: PeerEvent,
    role: Role,
    connected: &mut bool,
    errors: &mut Vec<String>,
) {
    match event {
        PeerEvent::NegotiationNeeded => {
            if role == Role::Offerer {
                own.create_offer();
            }
        }
        PeerEvent::LocalDescription { kind, sdp } => {
            if let Err(error) = peer.set_remote_description(&kind, &sdp) {
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
        } => peer.add_ice_candidate(mline_index, &candidate),
        PeerEvent::ConnectionState(state) => *connected = state.is_connected(),
        PeerEvent::Error(error) => errors.push(error),
        // A capture device failing would make a test pass while the call
        // it claims to prove carried nothing, so it is an error here.
        PeerEvent::CaptureFailed { what, detail } => {
            errors.push(format!("the {what} failed: {detail}"));
        }
        PeerEvent::IceState(_) | PeerEvent::RemoteTrack { .. } => {}
    }
}

/// Drive both ends for as long as asked, reporting anything that went wrong.
async fn pump(
    alice: &PeerConnection,
    bob: &PeerConnection,
    alice_events: &mut UnboundedReceiver<PeerEvent>,
    bob_events: &mut UnboundedReceiver<PeerEvent>,
    until: tokio::time::Instant,
    state: &mut (bool, bool),
    errors: &mut Vec<String>,
) {
    while tokio::time::Instant::now() < until {
        tokio::select! {
            Some(event) = alice_events.recv() => {
                handle(alice, bob, event, Role::Offerer, &mut state.0, errors);
            }
            Some(event) = bob_events.recv() => {
                handle(bob, alice, event, Role::Answerer, &mut state.1, errors);
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn our_own_camera_can_be_watched_without_a_call() {
    // A preview is worth having before anybody answers, and it must not need
    // a second look at the camera: a device opens once, so the picture shown
    // is the one already being prepared for sending.
    kestrel_media::init().expect("GStreamer should initialise");

    let (peer, _events) =
        PeerConnection::new("alone", Sending::audio_video(), &Source::Test, None).unwrap();

    let (sink, frames) = counting_sink();
    peer.set_self_view_sink(sink)
        .expect("a self view should be accepted");

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        frames.load(Ordering::Relaxed) > 0,
        "nothing reached the self view"
    );

    peer.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sink_offered_before_the_call_is_given_the_frames() {
    kestrel_media::init().expect("GStreamer should initialise");
    let sending = Sending::audio_video();

    let (alice, mut alice_events) =
        PeerConnection::new("alice", sending, &Source::Test, None).unwrap();
    let (bob, mut bob_events) = PeerConnection::new("bob", sending, &Source::Test, None).unwrap();

    // Offered before anything is negotiated, let alone decoded.
    let (sink, frames) = counting_sink();
    alice
        .set_video_sink(sink)
        .expect("offering a sink early should be accepted");

    let mut state = (false, false);
    let mut errors = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    pump(
        &alice,
        &bob,
        &mut alice_events,
        &mut bob_events,
        deadline,
        &mut state,
        &mut errors,
    )
    .await;

    assert!(errors.is_empty(), "the pipeline reported: {errors:#?}");
    assert!(state.0 && state.1, "the peers did not connect");
    assert!(
        frames.load(Ordering::Relaxed) > 0,
        "the sink the caller supplied was never given a frame"
    );

    alice.close();
    bob.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sink_offered_once_video_is_flowing_replaces_what_was_there() {
    kestrel_media::init().expect("GStreamer should initialise");
    let sending = Sending::audio_video();

    let (alice, mut alice_events) =
        PeerConnection::new("alice", sending, &Source::Test, None).unwrap();
    let (bob, mut bob_events) = PeerConnection::new("bob", sending, &Source::Test, None).unwrap();

    let mut state = (false, false);
    let mut errors = Vec::new();

    // Let the call settle first, so the chain exists and is carrying frames
    // into the discard sink the engine built for itself.
    let settled = tokio::time::Instant::now() + Duration::from_secs(20);
    pump(
        &alice,
        &bob,
        &mut alice_events,
        &mut bob_events,
        settled,
        &mut state,
        &mut errors,
    )
    .await;
    assert!(state.0 && state.1, "the peers did not connect");

    let (sink, frames) = counting_sink();
    alice
        .set_video_sink(sink)
        .expect("offering a sink late should be accepted");

    let after = tokio::time::Instant::now() + Duration::from_secs(8);
    pump(
        &alice,
        &bob,
        &mut alice_events,
        &mut bob_events,
        after,
        &mut state,
        &mut errors,
    )
    .await;

    assert!(errors.is_empty(), "the pipeline reported: {errors:#?}");
    assert!(
        frames.load(Ordering::Relaxed) > 0,
        "the replacement sink was never given a frame"
    );

    alice.close();
    bob.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn video_can_be_shut_off_without_disturbing_the_call() {
    // Denying somebody your picture must not renegotiate or drop the call:
    // the far end keeps its side and simply stops receiving frames.
    kestrel_media::init().expect("GStreamer should initialise");
    let sending = Sending::audio_video();

    let (alice, mut alice_events) =
        PeerConnection::new("alice", sending, &Source::Test, None).unwrap();
    let (bob, mut bob_events) = PeerConnection::new("bob", sending, &Source::Test, None).unwrap();

    let (sink, frames) = counting_sink();
    bob.set_video_sink(sink).expect("a sink should be accepted");

    let mut state = (false, false);
    let mut errors = Vec::new();
    let settled = tokio::time::Instant::now() + Duration::from_secs(20);
    pump(
        &alice,
        &bob,
        &mut alice_events,
        &mut bob_events,
        settled,
        &mut state,
        &mut errors,
    )
    .await;
    assert!(state.0 && state.1, "the peers did not connect");
    assert!(
        frames.load(Ordering::Relaxed) > 0,
        "bob was not receiving alice's video to begin with"
    );

    alice.set_sending_video(false);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let before = frames.load(Ordering::Relaxed);
    let quiet = tokio::time::Instant::now() + Duration::from_secs(4);
    pump(
        &alice,
        &bob,
        &mut alice_events,
        &mut bob_events,
        quiet,
        &mut state,
        &mut errors,
    )
    .await;
    let after = frames.load(Ordering::Relaxed);

    assert_eq!(before, after, "frames kept arriving after the valve closed");
    assert!(errors.is_empty(), "the call was disturbed: {errors:#?}");
    assert!(
        state.0 && state.1,
        "the call dropped rather than went quiet"
    );

    // And it can be opened again.
    alice.set_sending_video(true);
    let again = tokio::time::Instant::now() + Duration::from_secs(6);
    pump(
        &alice,
        &bob,
        &mut alice_events,
        &mut bob_events,
        again,
        &mut state,
        &mut errors,
    )
    .await;
    assert!(
        frames.load(Ordering::Relaxed) > after,
        "video did not come back"
    );

    alice.close();
    bob.close();
}
