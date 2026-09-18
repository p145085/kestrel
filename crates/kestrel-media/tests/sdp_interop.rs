//! The SDP our media engine produces must survive the compact form.
//!
//! These two were built separately and tested separately: one generates SDP,
//! the other reduces it to a couple of hundred bytes and rebuilds it. If they
//! disagree about a single attribute, every call fails with nothing more
//! useful than "malformed session description".

use std::time::Duration;

use kestrel_media::{PeerConnection, PeerEvent, Sending, Source};
use kestrel_rtc_proto::sdp;

/// Ask a peer connection for an offer and wait for it.
async fn real_offer(sending: Sending) -> String {
    kestrel_media::init().expect("GStreamer should initialise");
    let (peer, mut events) =
        PeerConnection::new("offerer", sending, &Source::Test, None).expect("should build");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            Some(event) = events.recv() => match event {
                PeerEvent::NegotiationNeeded => peer.create_offer(),
                PeerEvent::LocalDescription { sdp, .. } => {
                    peer.close();
                    return sdp;
                }
                PeerEvent::Error(error) => panic!("pipeline failed: {error}"),
                _ => {}
            },
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
    panic!("no offer was produced");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_offer_reduces_to_the_compact_form() {
    let offer = real_offer(Sending::audio_video()).await;
    let compact = sdp::from_sdp(&offer).unwrap_or_else(|| {
        panic!("could not reduce a real offer:\n{offer}");
    });

    assert!(!compact.ice_ufrag.is_empty(), "no ICE username fragment");
    assert!(!compact.ice_pwd.is_empty(), "no ICE password");
    assert_ne!(compact.fingerprint, [0; 32], "no DTLS fingerprint");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuilt_offer_is_accepted_by_the_media_engine() {
    // The real proof: what comes out of the compact form has to be something
    // webrtcbin will actually take as a remote description.
    let offer = real_offer(Sending::audio_video()).await;
    let compact = sdp::from_sdp(&offer).expect("should reduce");
    let rebuilt = sdp::to_sdp(&compact);

    kestrel_media::init().unwrap();
    let (peer, _events) =
        PeerConnection::new("answerer", Sending::audio_video(), &Source::Test, None).unwrap();
    peer.set_remote_description("offer", &rebuilt)
        .unwrap_or_else(|error| panic!("rebuilt offer was refused: {error}\n{rebuilt}"));
    peer.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_audio_only_offer_reduces_too() {
    let offer = real_offer(Sending::audio_only()).await;
    let compact = sdp::from_sdp(&offer)
        .unwrap_or_else(|| panic!("could not reduce an audio-only offer:\n{offer}"));
    assert!(compact.video_ssrc.is_none());
}
