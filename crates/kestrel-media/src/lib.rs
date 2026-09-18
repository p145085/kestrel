//! The media engine: capture, encode, and the WebRTC peer connections.
//!
//! Everything that touches audio, video or `webrtcbin` lives here, and nothing
//! here knows about IRC. The signalling that connects two of these is somebody
//! else's problem, which is what lets both be tested on their own.

pub mod peer;

use std::sync::Once;

use gstreamer as gst;

pub use peer::{
    ConnectionState, IceState, PeerConnection, PeerEvent, Sending, Source, cameras,
};

/// Why the media engine could not do something.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    /// GStreamer would not start.
    #[error("GStreamer could not be initialised: {0}")]
    Init(String),
    /// A required element is missing from the installation.
    #[error("this GStreamer installation has no {0}")]
    MissingElement(&'static str),
    /// The pipeline could not be built or started.
    #[error("{0}")]
    Pipeline(String),
    /// A session description could not be parsed.
    #[error("malformed session description")]
    BadSdp,
}

static INIT: Once = Once::new();

/// Start GStreamer.
///
/// Safe to call repeatedly; only the first call does anything. It must happen
/// before any element is built, and on Windows before any Direct3D context is
/// created.
pub fn init() -> Result<(), MediaError> {
    let mut result = Ok(());
    INIT.call_once(|| {
        result = gst::init().map_err(|error| MediaError::Init(error.to_string()));
    });
    result
}

/// Whether every element a call needs is present.
///
/// Worth checking at start-up rather than when somebody presses call: a
/// missing plugin should be a clear message, not a failed call.
#[must_use]
pub fn missing_elements() -> Vec<&'static str> {
    const REQUIRED: &[&str] = &[
        "webrtcbin",
        "opusenc",
        "rtpopuspay",
        "vp8enc",
        "rtpvp8pay",
        "audioconvert",
        "audioresample",
        "videoconvert",
        "queue",
    ];
    REQUIRED
        .iter()
        .filter(|name| gst::ElementFactory::find(name).is_none())
        .copied()
        .collect()
}

/// The version of GStreamer in use.
#[must_use]
pub fn version() -> String {
    gst::version_string().to_string()
}

#[cfg(test)]
mod tests {
    use super::{init, missing_elements, version};

    #[test]
    fn gstreamer_starts_and_has_what_a_call_needs() {
        init().expect("GStreamer should initialise");
        assert!(
            version().contains("GStreamer"),
            "unexpected version string: {}",
            version()
        );

        let missing = missing_elements();
        assert!(
            missing.is_empty(),
            "this installation is missing: {missing:?}"
        );
    }

    #[test]
    fn initialising_twice_is_harmless() {
        assert!(init().is_ok());
        assert!(init().is_ok());
    }
}
