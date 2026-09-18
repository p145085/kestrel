//! One peer connection: a `webrtcbin` and the pipeline around it.
//!
//! The threading rules here are not stylistic. `webrtcbin` emits its signals
//! on GStreamer's own streaming threads, and those threads must not be made to
//! wait on anything. Every handler below does exactly one thing — push a
//! message into a channel — and all decisions happen on the receiving side.
//! Doing work inside a handler, or taking a lock across `emit_by_name`, is the
//! classic way to deadlock this stack.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_webrtc as gst_webrtc;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::MediaError;

/// What media a peer connection should send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sending {
    /// Send audio.
    pub audio: bool,
    /// Send video.
    pub video: bool,
}

impl Sending {
    /// Audio and video.
    #[must_use]
    pub fn audio_video() -> Self {
        Self {
            audio: true,
            video: true,
        }
    }

    /// Audio only.
    #[must_use]
    pub fn audio_only() -> Self {
        Self {
            audio: true,
            video: false,
        }
    }
}

/// Where a peer connection's media comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Real capture devices.
    Devices,
    /// Generated test patterns.
    ///
    /// What makes the media path testable without a camera, a microphone, or
    /// a person sitting in front of either.
    Test,
}

/// How far along the overall connection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Nothing has started.
    New,
    /// Working on it.
    Connecting,
    /// Media can flow.
    Connected,
    /// Lost, possibly temporarily.
    Disconnected,
    /// Gave up.
    Failed,
    /// Shut down.
    Closed,
    /// Something this build does not know about.
    Unknown,
}

impl ConnectionState {
    fn from_webrtc(state: gst_webrtc::WebRTCPeerConnectionState) -> Self {
        match state {
            gst_webrtc::WebRTCPeerConnectionState::New => Self::New,
            gst_webrtc::WebRTCPeerConnectionState::Connecting => Self::Connecting,
            gst_webrtc::WebRTCPeerConnectionState::Connected => Self::Connected,
            gst_webrtc::WebRTCPeerConnectionState::Disconnected => Self::Disconnected,
            gst_webrtc::WebRTCPeerConnectionState::Failed => Self::Failed,
            gst_webrtc::WebRTCPeerConnectionState::Closed => Self::Closed,
            _ => Self::Unknown,
        }
    }

    /// Whether media can flow.
    #[must_use]
    pub fn is_connected(self) -> bool {
        self == Self::Connected
    }

    /// Whether this state is final, so a caller can stop waiting.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Closed)
    }
}

/// How far along ICE is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceState {
    /// Nothing has started.
    New,
    /// Trying candidate pairs.
    Checking,
    /// A pair works.
    Connected,
    /// Every pair has been tried and one works.
    Completed,
    /// No pair worked.
    ///
    /// Almost always means both ends are behind restrictive NATs and there is
    /// no relay configured.
    Failed,
    /// Connectivity lapsed.
    Disconnected,
    /// Shut down.
    Closed,
    /// Something this build does not know about.
    Unknown,
}

impl IceState {
    fn from_webrtc(state: gst_webrtc::WebRTCICEConnectionState) -> Self {
        match state {
            gst_webrtc::WebRTCICEConnectionState::New => Self::New,
            gst_webrtc::WebRTCICEConnectionState::Checking => Self::Checking,
            gst_webrtc::WebRTCICEConnectionState::Connected => Self::Connected,
            gst_webrtc::WebRTCICEConnectionState::Completed => Self::Completed,
            gst_webrtc::WebRTCICEConnectionState::Failed => Self::Failed,
            gst_webrtc::WebRTCICEConnectionState::Disconnected => Self::Disconnected,
            gst_webrtc::WebRTCICEConnectionState::Closed => Self::Closed,
            _ => Self::Unknown,
        }
    }
}

/// Something a peer connection reports.
#[derive(Debug, Clone)]
pub enum PeerEvent {
    /// The connection wants an offer or answer created.
    NegotiationNeeded,
    /// A local description is ready to send.
    LocalDescription {
        /// `offer` or `answer`.
        kind: String,
        /// The SDP.
        sdp: String,
    },
    /// A local ICE candidate is ready to send.
    IceCandidate {
        /// Which media section it belongs to.
        mline_index: u32,
        /// The candidate line.
        candidate: String,
    },
    /// The ICE connection state changed.
    IceState(IceState),
    /// The overall connection state changed.
    ConnectionState(ConnectionState),
    /// Media from the peer started arriving.
    RemoteTrack {
        /// `audio` or `video`.
        kind: String,
    },
    /// Something went wrong.
    Error(String),
}

/// One connection to one peer.
pub struct PeerConnection {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    /// Kept so that promise callbacks can report what they produced.
    events: mpsc::UnboundedSender<PeerEvent>,
}

impl std::fmt::Debug for PeerConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerConnection")
            .field("name", &self.pipeline.name())
            .finish_non_exhaustive()
    }
}

impl PeerConnection {
    /// Build a connection and start its pipeline.
    ///
    /// `stun` is optional: without it only host candidates are gathered, which
    /// works on a local network and nowhere else.
    pub fn new(
        name: &str,
        sending: Sending,
        source: Source,
        stun: Option<&str>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<PeerEvent>), MediaError> {
        let pipeline = gst::Pipeline::builder().name(name).build();

        let webrtc = gst::ElementFactory::make("webrtcbin")
            .name("webrtc")
            // Every implementation that matters speaks unified plan; the older
            // shape exists only for compatibility with things we do not talk to.
            .property_from_str("bundle-policy", "max-bundle")
            .build()
            .map_err(|_| MediaError::MissingElement("webrtcbin"))?;
        if let Some(stun) = stun {
            webrtc.set_property("stun-server", stun);
        }
        pipeline
            .add(&webrtc)
            .map_err(|_| MediaError::Pipeline("could not add webrtcbin".into()))?;

        if sending.audio {
            add_audio(&pipeline, &webrtc, source)?;
        }
        if sending.video {
            add_video(&pipeline, &webrtc, source)?;
        }

        let (events, receiver) = mpsc::unbounded_channel();
        connect_signals(&webrtc, &events);
        watch_bus(&pipeline, &events);

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| MediaError::Pipeline("could not start the pipeline".into()))?;

        Ok((
            Self {
                pipeline,
                webrtc,
                events,
            },
            receiver,
        ))
    }

    /// Ask for an offer. The result arrives as [`PeerEvent::LocalDescription`].
    pub fn create_offer(&self) {
        self.create_description("create-offer", "offer");
    }

    /// Ask for an answer to the remote offer already set.
    pub fn create_answer(&self) {
        self.create_description("create-answer", "answer");
    }

    fn create_description(&self, signal: &str, kind: &'static str) {
        let webrtc = self.webrtc.clone();
        let events = self.events.clone();
        let promise = gst::Promise::with_change_func(move |reply| {
            let Ok(Some(reply)) = reply else {
                let _ = events.send(PeerEvent::Error(format!("{kind} was not produced")));
                return;
            };
            let Ok(description) = reply.get::<gst_webrtc::WebRTCSessionDescription>(kind) else {
                let _ = events.send(PeerEvent::Error(format!(
                    "{kind} reply carried no session description"
                )));
                return;
            };

            // Setting our own description is what starts ICE gathering, which
            // is why it happens here rather than being left to the caller.
            webrtc.emit_by_name::<()>(
                "set-local-description",
                &[&description, &None::<gst::Promise>],
            );

            // And the caller has to be told, or the description never reaches
            // the peer and the call silently never connects.
            let sdp = description.sdp().as_text().unwrap_or_default();
            let _ = events.send(PeerEvent::LocalDescription {
                kind: kind.to_owned(),
                sdp,
            });
        });
        self.webrtc
            .emit_by_name::<()>(signal, &[&None::<gst::Structure>, &promise]);
    }

    /// Apply a description received from the peer.
    pub fn set_remote_description(&self, kind: &str, sdp: &str) -> Result<(), MediaError> {
        let message = gstreamer_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
            .map_err(|_| MediaError::BadSdp)?;
        let kind = match kind {
            "offer" => gst_webrtc::WebRTCSDPType::Offer,
            "answer" => gst_webrtc::WebRTCSDPType::Answer,
            _ => return Err(MediaError::BadSdp),
        };
        let description = gst_webrtc::WebRTCSessionDescription::new(kind, message);
        self.webrtc.emit_by_name::<()>(
            "set-remote-description",
            &[&description, &None::<gst::Promise>],
        );
        Ok(())
    }

    /// Add a candidate received from the peer.
    pub fn add_ice_candidate(&self, mline_index: u32, candidate: &str) {
        self.webrtc
            .emit_by_name::<()>("add-ice-candidate", &[&mline_index, &candidate]);
    }

    /// Statistics, for asserting that media is actually flowing.
    #[must_use]
    pub fn stats(&self) -> Option<gst::Structure> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let promise = gst::Promise::with_change_func(move |reply| {
            let stats = reply.ok().flatten().map(gst::StructureRef::to_owned);
            let _ = sender.send(stats);
        });
        self.webrtc
            .emit_by_name::<()>("get-stats", &[&None::<gst::Pad>, &promise]);
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .flatten()
    }

    /// Stop the pipeline and release its devices.
    pub fn close(&self) {
        if self.pipeline.set_state(gst::State::Null).is_err() {
            warn!("pipeline did not stop cleanly");
        }
    }
}

impl Drop for PeerConnection {
    /// Stops the pipeline.
    ///
    /// Without this a dropped connection keeps the camera light on and the
    /// streaming threads running, which users notice immediately.
    fn drop(&mut self) {
        self.close();
    }
}

/// Wire the signals that fire on GStreamer's streaming threads.
fn connect_signals(webrtc: &gst::Element, events: &mpsc::UnboundedSender<PeerEvent>) {
    let tx = events.clone();
    webrtc.connect("on-negotiation-needed", false, move |_| {
        let _ = tx.send(PeerEvent::NegotiationNeeded);
        None
    });

    let tx = events.clone();
    webrtc.connect("on-ice-candidate", false, move |values| {
        let mline_index = values.get(1).and_then(|v| v.get::<u32>().ok())?;
        let candidate = values.get(2).and_then(|v| v.get::<String>().ok())?;
        let _ = tx.send(PeerEvent::IceCandidate {
            mline_index,
            candidate,
        });
        None
    });

    let tx = events.clone();
    webrtc.connect_notify(Some("ice-connection-state"), move |element, _| {
        let state =
            element.property::<gst_webrtc::WebRTCICEConnectionState>("ice-connection-state");
        let _ = tx.send(PeerEvent::IceState(IceState::from_webrtc(state)));
    });

    let tx = events.clone();
    webrtc.connect_notify(Some("connection-state"), move |element, _| {
        let state = element.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
        let _ = tx.send(PeerEvent::ConnectionState(ConnectionState::from_webrtc(
            state,
        )));
    });

    // Incoming media arrives as a new pad. Decoding it is set up here, but the
    // work happens on GStreamer's threads, not this one.
    let tx = events.clone();
    webrtc.connect_pad_added(move |element, pad| {
        if pad.direction() != gst::PadDirection::Src {
            return;
        }
        let Some(pipeline) = element
            .parent()
            .and_then(|p| p.downcast::<gst::Pipeline>().ok())
        else {
            return;
        };
        match attach_receiver(&pipeline, pad) {
            Ok(kind) => {
                let _ = tx.send(PeerEvent::RemoteTrack { kind });
            }
            Err(error) => {
                let _ = tx.send(PeerEvent::Error(error.to_string()));
            }
        }
    });
}

/// Decode an incoming stream and throw it away.
///
/// Rendering is the UI's job: it creates the sink, because a `gdk::Paintable`
/// cannot cross threads while a `gst::Element` can. Until one is attached, the
/// stream still has to be consumed or the pipeline stalls.
fn attach_receiver(pipeline: &gst::Pipeline, pad: &gst::Pad) -> Result<String, MediaError> {
    let decode = gst::ElementFactory::make("decodebin3")
        .build()
        .or_else(|_| gst::ElementFactory::make("decodebin").build())
        .map_err(|_| MediaError::MissingElement("decodebin"))?;

    let sink_kind = std::sync::Arc::new(std::sync::Mutex::new(String::from("unknown")));
    let reported = std::sync::Arc::clone(&sink_kind);

    let pipeline_ref = pipeline.clone();
    decode.connect_pad_added(move |_, pad| {
        let kind = pad
            .current_caps()
            .and_then(|caps| caps.structure(0).map(|s| s.name().to_string()))
            .unwrap_or_default();
        let is_video = kind.starts_with("video");
        if let Ok(mut slot) = reported.lock() {
            let kind = if is_video { "video" } else { "audio" };
            kind.clone_into(&mut slot);
        }

        let Ok(sink) = build_discard_sink(&pipeline_ref, is_video) else {
            warn!("could not build a sink for {kind}");
            return;
        };
        if let Some(target) = sink.static_pad("sink")
            && pad.link(&target).is_err()
        {
            warn!("could not link decoded {kind}");
        }
    });

    pipeline
        .add(&decode)
        .map_err(|_| MediaError::Pipeline("could not add a decoder".into()))?;
    decode
        .sync_state_with_parent()
        .map_err(|_| MediaError::Pipeline("decoder would not start".into()))?;

    let target = decode
        .static_pad("sink")
        .ok_or(MediaError::Pipeline("decoder has no sink pad".into()))?;
    pad.link(&target)
        .map_err(|_| MediaError::Pipeline("could not link the incoming stream".into()))?;

    let kind = sink_kind.lock().map(|k| k.clone()).unwrap_or_default();
    debug!("receiving {kind}");
    Ok(kind)
}

fn build_discard_sink(
    pipeline: &gst::Pipeline,
    is_video: bool,
) -> Result<gst::Element, MediaError> {
    let convert = if is_video {
        "videoconvert"
    } else {
        "audioconvert"
    };
    let queue = gst::ElementFactory::make("queue")
        .build()
        .map_err(|_| MediaError::MissingElement("queue"))?;
    let convert = gst::ElementFactory::make(convert)
        .build()
        .map_err(|_| MediaError::MissingElement("converter"))?;
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .map_err(|_| MediaError::MissingElement("fakesink"))?;

    let elements = [&queue, &convert, &sink];
    pipeline
        .add_many(elements)
        .map_err(|_| MediaError::Pipeline("could not add a receive chain".into()))?;
    gst::Element::link_many(elements)
        .map_err(|_| MediaError::Pipeline("could not link a receive chain".into()))?;
    for element in elements {
        element
            .sync_state_with_parent()
            .map_err(|_| MediaError::Pipeline("receive chain would not start".into()))?;
    }
    Ok(queue)
}

/// Forward pipeline errors and warnings.
fn watch_bus(pipeline: &gst::Pipeline, events: &mpsc::UnboundedSender<PeerEvent>) {
    let Some(bus) = pipeline.bus() else {
        return;
    };
    let tx = events.clone();
    // A sync handler runs on the posting thread, so it must do nothing but
    // hand the message on. A watch would need a running GLib main loop, which
    // there is not one of here.
    bus.set_sync_handler(move |_, message| {
        match message.view() {
            gst::MessageView::Error(error) => {
                let _ = tx.send(PeerEvent::Error(error.error().to_string()));
            }
            gst::MessageView::Warning(warning) => {
                debug!("pipeline warning: {}", warning.error());
            }
            _ => {}
        }
        gst::BusSyncReply::Drop
    });
}

fn add_audio(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    source: Source,
) -> Result<(), MediaError> {
    let src = match source {
        Source::Test => gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .property_from_str("wave", "ticks")
            .build(),
        Source::Devices => gst::ElementFactory::make("wasapi2src")
            .property("low-latency", true)
            .build()
            .or_else(|_| gst::ElementFactory::make("autoaudiosrc").build()),
    }
    .map_err(|_| MediaError::MissingElement("audio source"))?;

    let convert = make("audioconvert")?;
    let resample = make("audioresample")?;
    let queue = make("queue")?;
    let encode = make("opusenc")?;
    let pay = gst::ElementFactory::make("rtpopuspay")
        .property("pt", 111u32)
        .build()
        .map_err(|_| MediaError::MissingElement("rtpopuspay"))?;

    let chain = [&src, &convert, &resample, &queue, &encode, &pay];
    pipeline
        .add_many(chain)
        .map_err(|_| MediaError::Pipeline("could not add the audio chain".into()))?;
    gst::Element::link_many(chain)
        .map_err(|_| MediaError::Pipeline("could not link the audio chain".into()))?;
    pay.link(webrtc)
        .map_err(|_| MediaError::Pipeline("could not link audio to webrtcbin".into()))?;
    Ok(())
}

fn add_video(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    source: Source,
) -> Result<(), MediaError> {
    let src = match source {
        Source::Test => gst::ElementFactory::make("videotestsrc")
            .property("is-live", true)
            .property_from_str("pattern", "ball")
            .build(),
        Source::Devices => gst::ElementFactory::make("mfvideosrc")
            .build()
            .or_else(|_| gst::ElementFactory::make("autovideosrc").build()),
    }
    .map_err(|_| MediaError::MissingElement("video source"))?;

    let convert = make("videoconvert")?;
    let queue = make("queue")?;
    // VP8 first: universally interoperable and no patent exposure. Realtime
    // deadline and no lag give an encoder that keeps up with a conversation
    // rather than one that produces a better picture too late to matter.
    let encode = gst::ElementFactory::make("vp8enc")
        .property("deadline", 1i64)
        .property("lag-in-frames", 0i32)
        .property_from_str("error-resilient", "partitions")
        .build()
        .map_err(|_| MediaError::MissingElement("vp8enc"))?;
    let pay = gst::ElementFactory::make("rtpvp8pay")
        .property("pt", 96u32)
        .build()
        .map_err(|_| MediaError::MissingElement("rtpvp8pay"))?;

    let chain = [&src, &convert, &queue, &encode, &pay];
    pipeline
        .add_many(chain)
        .map_err(|_| MediaError::Pipeline("could not add the video chain".into()))?;
    gst::Element::link_many(chain)
        .map_err(|_| MediaError::Pipeline("could not link the video chain".into()))?;
    pay.link(webrtc)
        .map_err(|_| MediaError::Pipeline("could not link video to webrtcbin".into()))?;
    Ok(())
}

fn make(factory: &'static str) -> Result<gst::Element, MediaError> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| MediaError::MissingElement(factory))
}
