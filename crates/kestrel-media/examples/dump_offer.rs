//! Print the SDP a real capture device produces, for diagnosing interop.

use std::time::Duration;

use kestrel_media::{PeerConnection, PeerEvent, Sending, Source};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    kestrel_media::init().expect("GStreamer should start");
    // Test patterns by default. Opening the real default camera is opt-in
    // because on Windows that device may be a paired phone, and enumerating
    // it sends a video request to it.
    let source = if std::env::args().any(|a| a == "--devices") {
        println!("opening your real microphone and camera");
        Source::Devices { camera: None }
    } else {
        Source::Test
    };

    let (peer, mut events) = match PeerConnection::new("dump", Sending::audio_video(), &source, None)
    {
        Ok(pair) => pair,
        Err(error) => {
            println!("COULD NOT BUILD: {error}");
            return;
        }
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            Some(event) = events.recv() => match event {
                PeerEvent::NegotiationNeeded => peer.create_offer(),
                PeerEvent::LocalDescription { sdp, .. } => {
                    println!("--- BEGIN SDP ---\n{sdp}--- END SDP ---");
                    peer.close();
                    return;
                }
                PeerEvent::Error(error) => println!("PIPELINE ERROR: {error}"),
                _ => {}
            },
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
    println!("NO OFFER PRODUCED");
}
