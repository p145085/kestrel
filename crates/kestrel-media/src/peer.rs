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
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{debug, error, warn};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Real capture devices.
    ///
    /// `camera` names one, matched case-insensitively against any part of the
    /// name [`cameras`] reports. Without it the system default is used, which
    /// on Windows is whatever the OS ranks first -- a paired phone, often
    /// enough, which is why the caller should usually choose deliberately.
    Devices {
        /// Which camera, or the system default.
        camera: Option<String>,
        /// Which microphone, or the system default.
        microphone: Option<String>,
    },
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
    /// A capture device stopped working.
    ///
    /// Reported apart from other failures because it is usually fixable and
    /// rarely fatal: the call carries on with whatever else it has.
    CaptureFailed {
        /// `camera` or `microphone`.
        what: &'static str,
        /// What GStreamer said.
        detail: String,
    },
    /// Something went wrong.
    Error(String),
}

/// Run a callback body without letting a panic escape into C.
///
/// Every handler here is called by GStreamer, from C, on one of its own
/// threads. A panic unwinding through a C frame is undefined behaviour, and
/// what it does in practice is end the process without a word -- which is the
/// worst way to find out about a bug, because there is nothing left to read.
/// Returning a fallback instead keeps the pipeline running and leaves a line
/// in the log saying where to look.
fn guarded<T>(what: &str, fallback: T, body: impl FnOnce() -> T) -> T {
    let Ok(value) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) else {
        error!("panicked inside {what}; the pipeline carries on without it");
        return fallback;
    };
    value
}

/// What the capture elements are called, so a failure can be attributed.
///
/// A bus error names the element that raised it, and "mfvideosrc0 reported an
/// internal data stream error" tells a user nothing they can act on. Knowing
/// it was the camera does.
const CAMERA: &str = "kestrel-camera";
/// As above, for the microphone.
const MICROPHONE: &str = "kestrel-microphone";

/// Whether a connection is still open, shared with the signal handlers.
///
/// Handlers run on GStreamer's own threads and add elements to a live
/// pipeline. The owner can close the connection at the same moment, and
/// tearing a pipeline down while another thread is adding to it corrupts the
/// heap. Both sides take this, so one cannot run inside the other, and a
/// handler that arrives after the close does nothing at all.
type Gate = Arc<Mutex<bool>>;

/// Where an incoming video stream is drawn.
///
/// The interface owns the sink, because a `gdk::Paintable` cannot leave the
/// thread that made it while a `gst::Element` can. So the element arrives here
/// from elsewhere, and may arrive before the stream does or after -- the first
/// is the ordinary case, the second happens when a call connects faster than
/// somebody can be asked where to put it.
#[derive(Clone, Default)]
struct VideoSlot(Arc<Mutex<Slot>>);

#[derive(Default)]
struct Slot {
    /// Handed over before there was anything to draw.
    waiting: Option<gst::Element>,
    /// The chain that is running, so a later sink can replace its end.
    live: Option<Live>,
}

/// A running video chain, cut at the point where the sink attaches.
struct Live {
    /// The converter feeding the sink. Its source pad is where we re-plug.
    convert: gst::Element,
    /// What is drawing at the moment.
    sink: gst::Element,
}

impl VideoSlot {
    /// Take whatever sink is waiting, if any.
    fn take_waiting(&self) -> Option<gst::Element> {
        self.0.lock().ok()?.waiting.take()
    }

    /// Remember the chain, so a sink offered later has somewhere to go.
    fn now_live(&self, convert: gst::Element, sink: gst::Element) {
        if let Ok(mut slot) = self.0.lock() {
            slot.live = Some(Live { convert, sink });
        }
    }
}

/// The capture devices a connection actually opened.
///
/// What was asked for and what was opened are not always the same: a name may
/// match nothing, a device may be busy, and the fallback is silent. Reporting
/// the real answer is the difference between "my setting did nothing" being a
/// guess and being a fact.
#[derive(Debug, Clone, Default)]
pub struct Devices {
    /// The camera, if one was opened.
    pub camera: Option<String>,
    /// The microphone, if one was opened.
    pub microphone: Option<String>,
}

/// An element held so it can be adjusted while the call runs.
///
/// Both controls have to exist before there is anything to control: adding a
/// valve or a volume to a pipeline that is already carrying media is delicate,
/// and doing it at the moment somebody clicks a menu is the worst time.
type Control = Arc<Mutex<Option<gst::Element>>>;

/// One connection to one peer.
pub struct PeerConnection {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    /// Kept so that promise callbacks can report what they produced.
    events: mpsc::UnboundedSender<PeerEvent>,
    gate: Gate,
    /// Where incoming video is drawn.
    video: VideoSlot,
    /// Where our own camera is drawn.
    self_view: VideoSlot,
    /// What was actually opened, as the system names it.
    devices: Devices,
    /// Shuts off the video we send to this peer.
    video_gate: Control,
    /// Silences the audio we receive from this peer.
    audio_gate: Control,
    /// When the pipeline started playing.
    opened: std::time::Instant,
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
        source: &Source,
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

        let mut microphone = None;
        if sending.audio {
            microphone = add_audio(&pipeline, &webrtc, source)?;
        }
        let self_view = VideoSlot::default();
        let video_gate: Control = Arc::new(Mutex::new(None));
        let audio_gate: Control = Arc::new(Mutex::new(None));
        let mut camera = None;
        if sending.video {
            camera = add_video(&pipeline, &webrtc, source, &self_view, &video_gate)?;
        }

        let (events, receiver) = mpsc::unbounded_channel();
        let gate: Gate = Arc::new(Mutex::new(true));
        let video = VideoSlot::default();
        connect_signals(&webrtc, &events, &gate, &video, &audio_gate);
        watch_bus(&pipeline, &events);

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| MediaError::Pipeline("could not start the pipeline".into()))?;

        Ok((
            Self {
                pipeline,
                webrtc,
                events,
                gate,
                video,
                self_view,
                devices: Devices { camera, microphone },
                video_gate,
                audio_gate,
                opened: std::time::Instant::now(),
            },
            receiver,
        ))
    }

    /// Draw this peer's video into the given sink.
    ///
    /// The sink is built by whoever owns the display and handed over as a
    /// plain element, which is the only part of a video widget that may cross
    /// a thread boundary. Safe to call at any point: before the stream exists
    /// it is held, and afterwards it replaces whatever is drawing.
    pub fn set_video_sink(&self, sink: gst::Element) -> Result<(), MediaError> {
        self.replace_in(&self.video, sink)
    }

    /// Put a sink into one of this connection's video slots.
    fn replace_in(&self, slot: &VideoSlot, sink: gst::Element) -> Result<(), MediaError> {
        let Ok(mut slot) = slot.0.lock() else {
            return Err(MediaError::Pipeline("the video slot is poisoned".into()));
        };

        let Some(live) = slot.live.as_mut() else {
            // Nothing is decoding yet, so there is nothing to re-plug. The
            // stream will pick this up when it arrives.
            slot.waiting = Some(sink);
            return Ok(());
        };

        let Some(source) = live.convert.static_pad("src") else {
            return Err(MediaError::Pipeline(
                "the converter has no source pad".into(),
            ));
        };

        // Re-plugged from an idle probe rather than directly: the pad is
        // carrying frames on a streaming thread, and unlinking it from under
        // that thread is how a running pipeline turns into a flow error.
        let pipeline = self.pipeline.clone();
        let old = live.sink.clone();
        let new = sink.clone();
        let gate = Arc::clone(&self.gate);
        source.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
            guarded("a video sink swap", gst::PadProbeReturn::Remove, || {
                let Ok(open) = gate.lock() else {
                    return gst::PadProbeReturn::Remove;
                };
                if !*open {
                    return gst::PadProbeReturn::Remove;
                }
                if let Err(error) = replace_sink(&pipeline, pad, &old, &new) {
                    warn!("could not attach the video sink: {error}");
                }
                gst::PadProbeReturn::Remove
            })
        });

        live.sink = sink;
        Ok(())
    }

    /// Draw what our own camera is seeing into the given sink.
    ///
    /// Branches off before the encoder, so what is shown is the picture being
    /// sent rather than a second look at the camera: a device can only be
    /// opened once, and a preview that opened it again would take the call's
    /// camera away from it.
    pub fn set_self_view_sink(&self, sink: gst::Element) -> Result<(), MediaError> {
        self.replace_in(&self.self_view, sink)
    }

    /// Whether to send this peer any video.
    ///
    /// Shut off at the valve rather than by renegotiating: the far end keeps
    /// its side of the call and simply stops receiving pictures, which is what
    /// "you may not see me" should look like from both ends.
    pub fn set_sending_video(&self, sending: bool) {
        if let Ok(gate) = self.video_gate.lock()
            && let Some(valve) = gate.as_ref()
        {
            valve.set_property("drop", !sending);
        }
    }

    /// Whether to play the audio this peer sends.
    ///
    /// Silenced here rather than by asking them to stop: what you listen to is
    /// your business and needs nobody's cooperation.
    pub fn set_hearing_audio(&self, hearing: bool) {
        if let Ok(gate) = self.audio_gate.lock()
            && let Some(volume) = gate.as_ref()
        {
            volume.set_property("mute", !hearing);
        }
    }

    /// What this connection actually captures from.
    #[must_use]
    pub fn devices(&self) -> &Devices {
        &self.devices
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
            guarded("a description callback", (), || {
                let Ok(Some(reply)) = reply else {
                    let _ = events.send(PeerEvent::Error(format!("{kind} was not produced")));
                    return;
                };
                let Ok(description) = reply.get::<gst_webrtc::WebRTCSessionDescription>(kind)
                else {
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
        {
            // Taken only to mark the connection shut, never held across the
            // teardown itself: stopping a pipeline waits for its streaming
            // threads, and a thread blocked on this lock would never stop.
            let mut open = match self.gate.lock() {
                Ok(open) => open,
                Err(poisoned) => poisoned.into_inner(),
            };
            if !*open {
                return;
            }
            *open = false;
        }

        // Down through READY rather than straight to NULL. Dropping a playing
        // pipeline to NULL in one step releases webrtcbin's ICE sockets while
        // its `nicesrc` threads are still pushing, which corrupts the heap if
        // a call is torn down moments after gathering starts -- hanging up
        // immediately after dialling does exactly that. Pausing first unlocks
        // those threads and waits for them before anything is freed.
        self.settle_ice();

        if self.pipeline.set_state(gst::State::Null).is_err() {
            warn!("pipeline did not stop cleanly");
            return;
        }
        // Returning before the pipeline has actually stopped leaves its
        // threads running behind us, which is how a closed connection goes on
        // holding a camera.
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(5));
    }
}

impl PeerConnection {
    /// Wait, briefly, for ICE gathering to finish.
    ///
    /// Releasing webrtcbin's sockets while libnice is still gathering
    /// corrupts the heap, and hanging up moments after dialling does exactly
    /// that. Gathering host candidates takes milliseconds; a STUN round trip
    /// takes a few hundred. The wait is bounded because a hangup must not be
    /// held hostage by an unreachable STUN server -- and because leaving late
    /// is better than crashing, but not at any price.
    fn settle_ice(&self) {
        use gst_webrtc::WebRTCICEGatheringState as Gathering;

        // webrtcbin reports gathering complete well before its ICE transport
        // threads have settled, and tearing the pipeline down in that window
        // corrupts the heap rather than failing cleanly. There is no property
        // that marks the end of it, so a young connection is held open for the
        // remainder of its first second. Only a hangup that arrives within a
        // second of dialling waits at all; every real call is long past this.
        const YOUNG: std::time::Duration = std::time::Duration::from_millis(1200);
        if let Some(remaining) = YOUNG.checked_sub(self.opened.elapsed()) {
            std::thread::sleep(remaining);
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if self.webrtc.property::<Gathering>("ice-gathering-state") != Gathering::Gathering {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        warn!("closing while ICE was still gathering");
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
fn connect_signals(
    webrtc: &gst::Element,
    events: &mpsc::UnboundedSender<PeerEvent>,
    gate: &Gate,
    video: &VideoSlot,
    audio_gate: &Control,
) {
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
    let gate = Arc::clone(gate);
    let video = video.clone();
    let audio_gate = audio_gate.clone();
    webrtc.connect_pad_added(move |element, pad| {
        guarded("an incoming stream", (), || {
            if pad.direction() != gst::PadDirection::Src {
                return;
            }
            let Ok(open) = gate.lock() else {
                return;
            };
            if !*open {
                return;
            }
            let Some(pipeline) = element
                .parent()
                .and_then(|p| p.downcast::<gst::Pipeline>().ok())
            else {
                return;
            };
            if let Err(error) = attach_receiver(&pipeline, pad, &tx, &gate, &video, &audio_gate) {
                let _ = tx.send(PeerEvent::Error(error.to_string()));
            }
        });
    });
}

/// Decode an incoming stream and throw it away.
///
/// Rendering is the UI's job: it creates the sink, because a `gdk::Paintable`
/// cannot cross threads while a `gst::Element` can. Until one is attached, the
/// stream still has to be consumed or the pipeline stalls.
fn attach_receiver(
    pipeline: &gst::Pipeline,
    pad: &gst::Pad,
    events: &mpsc::UnboundedSender<PeerEvent>,
    gate: &Gate,
    video: &VideoSlot,
    audio_gate: &Control,
) -> Result<(), MediaError> {
    // Plain `decodebin`, deliberately. The pad carries `application/x-rtp`,
    // and `decodebin3` does not autoplug RTP depayloaders: it accepts the
    // link, never produces a source pad, and everything upstream of it
    // eventually fails with `not-linked`, which surfaces as webrtcbin's own
    // `nicesrc` reporting an internal data stream error.
    let decode = gst::ElementFactory::make("decodebin")
        .build()
        .map_err(|_| MediaError::MissingElement("decodebin"))?;

    let pipeline_ref = pipeline.clone();
    let tx = events.clone();
    // Cloned rather than borrowed: this fires later, on another thread, and
    // possibly after the owner has closed the connection.
    let gate = Arc::clone(gate);
    let video = video.clone();
    let audio_gate = audio_gate.clone();
    decode.connect_pad_added(move |_, pad| {
        guarded("a decoded stream", (), || {
            let Ok(open) = gate.lock() else {
                return;
            };
            if !*open {
                return;
            }
            let caps = pad
                .current_caps()
                .and_then(|caps| caps.structure(0).map(|s| s.name().to_string()))
                .unwrap_or_default();
            let is_video = caps.starts_with("video");

            // Video goes wherever the interface asked, if it has asked yet.
            let wanted = if is_video { video.take_waiting() } else { None };
            match build_receive_chain(&pipeline_ref, is_video, wanted) {
                Ok(chain) => {
                    if is_video {
                        video.now_live(chain.convert, chain.sink);
                    } else if let Ok(mut held) = audio_gate.lock() {
                        (*held).clone_from(&chain.volume);
                    }
                    let linked = chain
                        .head
                        .static_pad("sink")
                        .is_some_and(|target| pad.link(&target).is_ok());
                    if linked {
                        // Reported from here rather than when the stream was
                        // linked, because until decoding starts there is nothing
                        // to name: the kind is only known once caps are.
                        let kind = if is_video { "video" } else { "audio" };
                        let _ = tx.send(PeerEvent::RemoteTrack {
                            kind: kind.to_owned(),
                        });
                    } else {
                        warn!("could not link decoded {caps}");
                    }
                }
                Err(error) => {
                    let _ = tx.send(PeerEvent::Error(error.to_string()));
                }
            }
        });
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

    debug!("decoding an incoming stream");
    Ok(())
}

/// The parts of a receive chain worth keeping hold of.
struct Chain {
    /// Where the decoded stream is linked in.
    head: gst::Element,
    /// The converter feeding the sink.
    convert: gst::Element,
    /// Where it ends up.
    sink: gst::Element,
    /// For audio, what can silence it without asking the sender to stop.
    volume: Option<gst::Element>,
}

/// Build the chain that consumes one decoded stream.
///
/// Without a sink of its own it ends in `fakesink`: a stream still has to be
/// consumed or the pipeline stalls, and rendering is the interface's business
/// rather than this crate's.
fn build_receive_chain(
    pipeline: &gst::Pipeline,
    is_video: bool,
    wanted: Option<gst::Element>,
) -> Result<Chain, MediaError> {
    let queue = make("queue")?;
    let convert = make(if is_video {
        "videoconvert"
    } else {
        "audioconvert"
    })?;

    // Audio is resampled because a device rarely runs at whatever rate the
    // far end encoded at, and a sink that cannot take the rate it is given
    // simply fails to negotiate.
    let resample = if is_video {
        None
    } else {
        Some(make("audioresample")?)
    };
    // Deafening somebody is a decision about what you listen to, so it belongs
    // on the receiving side and needs nobody's cooperation.
    let volume = if is_video {
        None
    } else {
        Some(make("volume")?)
    };

    let sink = match wanted {
        Some(sink) => sink,
        // Video has nowhere to go until the interface says where, and is
        // discarded meanwhile; audio has an obvious destination and no reason
        // to wait for anybody to choose it.
        None if is_video => gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
            .map_err(|_| MediaError::MissingElement("fakesink"))?,
        None => make("autoaudiosink").or_else(|_| {
            warn!("no audio output; the call will be silent");
            gst::ElementFactory::make("fakesink")
                .property("sync", false)
                .build()
                .map_err(|_| MediaError::MissingElement("fakesink"))
        })?,
    };

    let mut elements = vec![&queue, &convert];
    if let Some(resample) = &resample {
        elements.push(resample);
    }
    if let Some(volume) = &volume {
        elements.push(volume);
    }
    elements.push(&sink);

    pipeline
        .add_many(&elements)
        .map_err(|_| MediaError::Pipeline("could not add a receive chain".into()))?;
    gst::Element::link_many(&elements)
        .map_err(|_| MediaError::Pipeline("could not link a receive chain".into()))?;
    for element in &elements {
        element
            .sync_state_with_parent()
            .map_err(|_| MediaError::Pipeline("receive chain would not start".into()))?;
    }

    Ok(Chain {
        head: queue,
        // The cut point for a later sink swap is whatever feeds it.
        convert: volume.clone().or(resample).unwrap_or(convert),
        sink,
        volume,
    })
}

/// Put a different sink on the end of a running chain.
///
/// Called from an idle probe, so the pad is known not to be carrying anything
/// at this moment and the swap cannot tear a buffer in half.
fn replace_sink(
    pipeline: &gst::Pipeline,
    source: &gst::Pad,
    old: &gst::Element,
    new: &gst::Element,
) -> Result<(), MediaError> {
    if let Some(target) = old.static_pad("sink") {
        let _ = source.unlink(&target);
    }
    // Stopped before it leaves the pipeline: removing a playing element
    // leaves it running with nowhere to send what it produces.
    let _ = old.set_state(gst::State::Null);
    let _ = pipeline.remove(old);

    pipeline
        .add(new)
        .map_err(|_| MediaError::Pipeline("could not add the video sink".into()))?;
    new.sync_state_with_parent()
        .map_err(|_| MediaError::Pipeline("the video sink would not start".into()))?;

    let target = new.static_pad("sink").ok_or(MediaError::Pipeline(
        "the video sink has no sink pad".into(),
    ))?;
    source
        .link(&target)
        .map_err(|_| MediaError::Pipeline("could not link the video sink".into()))?;
    Ok(())
}

/// Forward pipeline errors and warnings.
fn watch_bus(pipeline: &gst::Pipeline, events: &mpsc::UnboundedSender<PeerEvent>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    let reported = Arc::new(AtomicBool::new(false));
    let Some(bus) = pipeline.bus() else {
        return;
    };
    let tx = events.clone();
    // A sync handler runs on the posting thread, so it must do nothing but
    // hand the message on. A watch would need a running GLib main loop, which
    // there is not one of here.
    bus.set_sync_handler(move |_, message| {
        guarded("the pipeline bus", gst::BusSyncReply::Drop, || {
            match message.view() {
                gst::MessageView::Error(error) if reported.swap(true, Ordering::Relaxed) => {
                    // GStreamer cascades: one element failing makes its neighbours
                    // fail too. The first is the one worth reporting; the rest are
                    // a description of the wreckage.
                    debug!("further pipeline error: {}", error.error());
                }
                gst::MessageView::Error(error) => {
                    // Name the element and keep the detail. "Internal data stream
                    // error" on its own says nothing about which part of a
                    // dozen-element pipeline gave up.
                    let source = message.src().map_or_else(
                        || "pipeline".to_owned(),
                        |src| src.path_string().to_string(),
                    );
                    let detail = error.debug().map(|d| format!(" ({d})")).unwrap_or_default();
                    let what = if source.contains(CAMERA) {
                        Some("camera")
                    } else if source.contains(MICROPHONE) {
                        Some("microphone")
                    } else {
                        None
                    };

                    let _ = tx.send(match what {
                        Some(what) => PeerEvent::CaptureFailed {
                            what,
                            detail: error.error().to_string(),
                        },
                        None => PeerEvent::Error(format!("{source}: {}{detail}", error.error())),
                    });
                }
                gst::MessageView::Warning(warning) => {
                    debug!("pipeline warning: {}", warning.error());
                }
                _ => {}
            }
            gst::BusSyncReply::Drop
        })
    });
}

/// The microphones this machine offers.
#[must_use]
pub fn microphones() -> Vec<String> {
    audio_devices()
        .iter()
        .map(|device| device.display_name().to_string())
        .collect()
}

/// Every device the monitor considers an audio source.
fn audio_devices() -> Vec<gst::Device> {
    devices_of_kind("Audio")
}

/// The cameras this machine offers, in the order the system ranks them.
///
/// Worth showing before opening anything: the first entry is what a default
/// capture would take, and on Windows that can be a paired phone rather than
/// anything plugged into the machine.
#[must_use]
pub fn cameras() -> Vec<String> {
    video_devices()
        .iter()
        .map(|device| device.display_name().to_string())
        .collect()
}

/// Every device the monitor considers a video source.
fn video_devices() -> Vec<gst::Device> {
    devices_of_kind("Video")
}

/// Capture devices of one kind, best backend first and one entry per name.
///
/// Both orderings of the class are accepted because Windows reports
/// `Video/Source` for some backends and `Source/Video` for others, and a
/// filter matching only one of them silently hides half the devices.
fn devices_of_kind(kind: &str) -> Vec<gst::Device> {
    let monitor = gst::DeviceMonitor::new();
    if monitor.start().is_err() {
        return Vec::new();
    }
    let found = monitor.devices().into_iter().filter(|device| {
        let class = device.device_class();
        class.contains(kind) && class.contains("Source")
    });

    // Windows offers one physical camera through more than one backend, and
    // they are not equally good: the legacy Kernel Streaming source fails to
    // start on hardware that Media Foundation drives perfectly well. Keep the
    // best backend for each name, in the order the names first appeared, so
    // the list has one entry per camera and its first entry is a real default.
    let mut best: Vec<(u8, gst::Device)> = Vec::new();
    for device in found {
        let rank = backend_rank(&device);
        let name = device.display_name();
        match best
            .iter()
            .position(|(_, kept)| kept.display_name() == name)
        {
            Some(at) if rank < best[at].0 => best[at] = (rank, device),
            Some(_) => {}
            None => best.push((rank, device)),
        }
    }

    monitor.stop();
    best.into_iter().map(|(_, device)| device).collect()
}

/// How much we trust the backend behind a device. Lower is better.
///
/// Read from the device's own properties rather than by building its element:
/// constructing a legacy source merely to identify it prints a deprecation
/// warning, and this runs before every call.
fn backend_rank(device: &gst::Device) -> u8 {
    let api = device
        .properties()
        .and_then(|properties| properties.get::<String>("device.api").ok());
    match api.as_deref() {
        // Windows offers cameras through both Media Foundation and the older
        // Kernel Streaming source, and the latter fails to start on hardware
        // the former drives without complaint.
        Some("mediafoundation") => 0,
        // Elsewhere each camera has one backend, so everything ranks alike and
        // the system's own ordering is left untouched.
        _ => 1,
    }
}

/// Build a source for whichever device the system ranks first.
fn first_device(devices: &[gst::Device]) -> Option<(gst::Element, String)> {
    let device = devices.first()?;
    let name = device.display_name().to_string();
    debug!("opening the default device, {name}");
    Some((device.create_element(None).ok()?, name))
}

/// Build a source for the first device whose name contains `wanted`.
///
/// Returns `None` when nothing matches, leaving the caller to fall back to the
/// default rather than failing outright -- a camera unplugged since it was
/// chosen should not stop a call from happening at all.
fn open_device(devices: &[gst::Device], wanted: &str) -> Option<(gst::Element, String)> {
    let wanted = wanted.to_lowercase();
    let device = devices
        .iter()
        .find(|device| device.display_name().to_lowercase().contains(&wanted))?;
    let name = device.display_name().to_string();
    debug!("opening device {name}");
    Some((device.create_element(None).ok()?, name))
}

fn add_audio(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    source: &Source,
) -> Result<Option<String>, MediaError> {
    let mut opened = None;
    let src = match source {
        Source::Test => gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .property_from_str("wave", "ticks")
            .build(),
        Source::Devices { microphone, .. } => {
            let microphones = audio_devices();
            let chosen = match microphone.as_deref() {
                Some(wanted) => open_device(&microphones, wanted),
                None => first_device(&microphones),
            };
            match chosen {
                Some((src, name)) => {
                    opened = Some(name);
                    Ok(src)
                }
                None => gst::ElementFactory::make("wasapi2src")
                    .property("low-latency", true)
                    .build()
                    .or_else(|_| gst::ElementFactory::make("autoaudiosrc").build()),
            }
        }
    }
    .map_err(|_| MediaError::MissingElement("audio source"))?;
    src.set_property("name", MICROPHONE);

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
    Ok(opened)
}

fn add_video(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    source: &Source,
    self_view: &VideoSlot,
    gate: &Control,
) -> Result<Option<String>, MediaError> {
    let mut opened = None;
    let src = match source {
        Source::Test => gst::ElementFactory::make("videotestsrc")
            .property("is-live", true)
            .property_from_str("pattern", "ball")
            .build(),
        Source::Devices { camera, .. } => {
            // Named or not, the camera comes from the device monitor, so what
            // a call opens is the same list `cameras` reports and the first
            // entry really is the default. Reaching for a bare `mfvideosrc`
            // instead would quietly pick whatever the OS ranks first, which
            // is not necessarily anything attached to this machine.
            let cameras = video_devices();
            let chosen = match camera.as_deref() {
                Some(wanted) => open_device(&cameras, wanted),
                None => first_device(&cameras),
            };
            match chosen {
                Some((src, name)) => {
                    opened = Some(name);
                    Ok(src)
                }
                None => gst::ElementFactory::make("mfvideosrc")
                    .build()
                    .or_else(|_| gst::ElementFactory::make("autovideosrc").build()),
            }
        }
    }
    .map_err(|_| MediaError::MissingElement("video source"))?;
    src.set_property("name", CAMERA);

    // Cameras advertise their best format first, and for most webcams that is
    // MJPEG at the highest resolution they manage -- which `videoconvert`
    // cannot accept at all, so negotiation fails and the source errors out
    // before a single frame is sent. Asking for raw video picks the format
    // the rest of the chain can actually use.
    let raw = gst::ElementFactory::make("capsfilter")
        .property("caps", gst::Caps::builder("video/x-raw").build())
        .build()
        .map_err(|_| MediaError::MissingElement("capsfilter"))?;

    let convert = make("videoconvert")?;
    let scale = make("videoscale")?;

    // A conversation, not a broadcast: 480p is what the mesh budget in the
    // design assumes, and a software VP8 encoder keeps up with it. The frame
    // rate is left to the camera, since not every one offers 30 here.
    let size = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("width", 640i32)
                .field("height", 480i32)
                .build(),
        )
        .build()
        .map_err(|_| MediaError::MissingElement("capsfilter"))?;

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

    // Split before the encoder so the preview shows the picture being sent.
    // Looking at the camera a second time is not an option: a device opens
    // once, and a preview that opened it again would take it from the call.
    let tee = make("tee")?;

    // Between the tee and the encoder, so shutting it off stops the pictures
    // reaching this peer while the preview carries on showing what the camera
    // sees. Nothing is renegotiated, so the call is undisturbed.
    let valve = make("valve")?;
    if let Ok(mut held) = gate.lock() {
        *held = Some(valve.clone());
    }

    let head = [&src, &raw, &convert, &scale, &size, &tee];
    let encoding = [&valve, &queue, &encode, &pay];
    pipeline
        .add_many(head)
        .map_err(|_| MediaError::Pipeline("could not add the video chain".into()))?;
    pipeline
        .add_many(encoding)
        .map_err(|_| MediaError::Pipeline("could not add the encoder".into()))?;
    gst::Element::link_many(head)
        .map_err(|_| MediaError::Pipeline("could not link the video chain".into()))?;
    gst::Element::link_many(encoding)
        .map_err(|_| MediaError::Pipeline("could not link the encoder".into()))?;

    link_tee(&tee, &valve)?;
    pay.link(webrtc)
        .map_err(|_| MediaError::Pipeline("could not link video to webrtcbin".into()))?;

    // The preview branch, ending nowhere until somebody says where. Built
    // whether or not it will ever be looked at, because adding a tee branch to
    // a running pipeline is far more delicate than leaving one idling.
    match add_preview_branch(pipeline, &tee) {
        Ok((convert, sink)) => self_view.now_live(convert, sink),
        // A call without a preview is worth having; a call that would not
        // start because of one is not.
        Err(error) => warn!("no self view: {error}"),
    }
    Ok(opened)
}

/// Attach a branch of a tee to an element.
fn link_tee(tee: &gst::Element, to: &gst::Element) -> Result<(), MediaError> {
    let source = tee
        .request_pad_simple("src_%u")
        .ok_or(MediaError::Pipeline("the tee has no spare branch".into()))?;
    let target = to.static_pad("sink").ok_or(MediaError::Pipeline(
        "nothing to attach the branch to".into(),
    ))?;
    source
        .link(&target)
        .map_err(|_| MediaError::Pipeline("could not attach a tee branch".into()))?;
    Ok(())
}

/// Build the branch that shows our own camera, ending in a discard sink.
///
/// Returns the converter and the sink, which is where a real one is later
/// plugged in: the same cut used for an incoming stream.
fn add_preview_branch(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
) -> Result<(gst::Element, gst::Element), MediaError> {
    let queue = make("queue")?;
    let convert = make("videoconvert")?;
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .map_err(|_| MediaError::MissingElement("fakesink"))?;

    let branch = [&queue, &convert, &sink];
    pipeline
        .add_many(branch)
        .map_err(|_| MediaError::Pipeline("could not add the preview branch".into()))?;
    gst::Element::link_many(branch)
        .map_err(|_| MediaError::Pipeline("could not link the preview branch".into()))?;
    link_tee(tee, &queue)?;

    Ok((convert, sink))
}

fn make(factory: &'static str) -> Result<gst::Element, MediaError> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| MediaError::MissingElement(factory))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a receive chain in a throwaway pipeline and say where it ends.
    fn sink_factory_for(is_video: bool) -> String {
        crate::init().expect("GStreamer should initialise");
        let pipeline = gst::Pipeline::new();
        let chain = build_receive_chain(&pipeline, is_video, None).expect("should build");
        let name = chain
            .sink
            .factory()
            .map_or_else(|| "none".to_owned(), |f| f.name().to_string());
        let _ = pipeline.set_state(gst::State::Null);
        name
    }

    #[test]
    fn asking_for_a_device_by_name_gets_that_device() {
        // Selection falls back silently when a name matches nothing, which is
        // indistinguishable from the choice having been ignored. Building the
        // element does not open the hardware, so this costs nothing.
        crate::init().expect("GStreamer should initialise");

        for kind in [video_devices(), audio_devices()] {
            for device in &kind {
                let wanted = device.display_name().to_string();
                let Some((_, opened)) = open_device(&kind, &wanted) else {
                    panic!("asking for {wanted} found nothing");
                };
                assert_eq!(
                    opened, wanted,
                    "asking for {wanted} opened {opened} instead"
                );
            }
        }
    }

    #[test]
    fn a_name_that_matches_nothing_is_refused_rather_than_substituted() {
        crate::init().expect("GStreamer should initialise");
        assert!(
            open_device(&video_devices(), "no such camera exists anywhere").is_none(),
            "a miss has to be visible, or the caller cannot fall back deliberately"
        );
    }

    #[test]
    fn incoming_audio_reaches_a_speaker() {
        // It was decoded and thrown away, which is audible as a call that
        // connects, reports the other side's audio, and stays silent.
        let sink = sink_factory_for(false);
        assert_ne!(
            sink, "fakesink",
            "audio must end somewhere that makes a sound"
        );
    }

    #[test]
    fn incoming_video_waits_for_somewhere_to_be_drawn() {
        // The opposite case: the engine has no business choosing a window, so
        // video is discarded until the interface supplies a sink.
        assert_eq!(sink_factory_for(true), "fakesink");
    }
}
