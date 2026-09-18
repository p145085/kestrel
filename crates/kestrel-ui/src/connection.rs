//! The half of the client that owns the socket.
//!
//! Runs on the tokio thread and never touches a widget. Everything it wants
//! shown leaves as an [`AppEvent`]; everything the user does arrives as a
//! [`UiCommand`]. That one-way split is what keeps GTK's `!Send` types and
//! tokio's work on opposite sides of a thread boundary without any locking.

use anyhow::Result;
use kestrel_client::{Calls, Level, Notice, TaggedMediaEvent};
use kestrel_net::{ClientEvent, ConnectConfig, Handle};
use kestrel_session::SessionConfig;
use kestrel_session::event::Event;
use tokio::sync::mpsc;

use crate::command::{self, Action};
use crate::event::{AppEvent, CallAction, Line, LineKind, SERVER_BUFFER, UiCommand};
use crate::translate::Translator;

/// Drive one connection until it ends.
///
/// Returns once the server is gone or the user has quit; the interface learns
/// which from the final [`AppEvent::Disconnected`].
/// How calls should be placed from this connection.
#[derive(Debug, Clone, Default)]
pub struct CallOptions {
    /// Use generated test media rather than real capture devices.
    pub test_media: bool,
    /// Which camera to open, by name.
    pub camera: Option<String>,
    /// Offer audio only.
    pub audio_only: bool,
}

pub async fn run(
    connect: ConnectConfig,
    session: SessionConfig,
    options: CallOptions,
    mut commands: mpsc::UnboundedReceiver<UiCommand>,
    events: async_channel::Sender<AppEvent>,
) {
    let server = format!("{}:{}", connect.host, connect.port);
    let _ = events
        .send(AppEvent::Connecting {
            server: server.clone(),
        })
        .await;

    let (handle, mut incoming) = match kestrel_net::connect(connect, session).await {
        Ok(pair) => pair,
        Err(error) => {
            let _ = events
                .send(AppEvent::Disconnected {
                    reason: format!("{error:#}"),
                })
                .await;
            return;
        }
    };

    let (media_tx, mut media_rx) = mpsc::unbounded_channel();
    let mut state = State::new(media_tx);
    if let Err(error) = kestrel_media::init() {
        show(
            &events,
            Line::error(format!("calls are unavailable: {error}")),
        )
        .await;
    }
    match kestrel_client::Store::in_config_directory() {
        Some(store) => {
            if let Err(error) = state.calls.remember_in(store) {
                show(
                    &events,
                    Line::error(format!("identity not remembered: {error:#}")),
                )
                .await;
            }
        }
        None => {
            show(
                &events,
                Line::error("nowhere to keep an identity; this run will look like a stranger"),
            )
            .await;
        }
    }
    state.configure(&options);
    state.flush(&events).await;

    let reason = loop {
        tokio::select! {
            event = incoming.recv() => {
                // The connection task ended without telling us why, which is
                // itself worth saying rather than closing in silence.
                let Some(event) = event else {
                    break "connection closed".to_owned();
                };
                if let Some(reason) = state.on_client_event(event, &handle, &events).await {
                    break reason;
                }
            }
            command = commands.recv() => {
                // The window is gone, so there is nobody left to talk to.
                let Some(command) = command else {
                    let _ = handle.quit("kestrel");
                    return;
                };
                if let Some(reason) = state.on_ui_command(command, &handle, &events).await {
                    break reason;
                }
            }
            media = media_rx.recv() => {
                let Some(media) = media else {
                    continue;
                };
                if let Err(error) = state.calls.on_media(&handle, media) {
                    show(&events, Line::error(error.to_string())).await;
                }
                state.flush(&events).await;
            }
        }
    };

    let _ = events.send(AppEvent::Disconnected { reason }).await;
}

/// What the connection half has to remember.
struct State {
    translator: Translator,
    /// Everything to do with calls.
    calls: Calls,
    /// What was last reported to the interface, so it is told only on change.
    reported: (bool, bool),
    /// Whether the server sends our own messages back to us.
    ///
    /// Decides whether saying something should also be shown locally. Getting
    /// this wrong in either direction is visible immediately: every message
    /// appears twice, or our own never appear at all.
    echoed: bool,
}

impl State {
    fn new(media_tx: mpsc::UnboundedSender<TaggedMediaEvent>) -> Self {
        Self {
            translator: Translator::new(),
            calls: Calls::new(media_tx),
            reported: (false, false),
            echoed: false,
        }
    }

    /// Apply the options a call should be placed with.
    fn configure(&mut self, options: &CallOptions) {
        if options.test_media {
            self.calls.use_test_media();
        }
        if let Some(camera) = &options.camera {
            self.calls.use_camera(camera.clone());
        }
        if options.audio_only {
            self.calls.audio_only();
        }
    }

    /// Send on everything the call machinery had to say.
    ///
    /// Called after anything that touches it, from one place per branch, so a
    /// notice cannot be lost by a caller forgetting to collect it.
    async fn flush(&mut self, events: &async_channel::Sender<AppEvent>) {
        for notice in self.calls.take_notices() {
            let _ = events
                .send(AppEvent::Line {
                    buffer: notice.target.clone().unwrap_or_default(),
                    line: line_of(&notice),
                })
                .await;
        }

        if self.calls.take_self_view_wanted() {
            let _ = events.send(AppEvent::SelfViewWanted).await;
        }
        for peer in self.calls.take_video_wanted() {
            let _ = events.send(AppEvent::VideoWanted { peer }).await;
        }

        let now = (self.calls.is_ringing(), self.calls.in_call());
        if now != self.reported {
            self.reported = now;
            let _ = events
                .send(AppEvent::CallState {
                    ringing: now.0,
                    active: now.1,
                })
                .await;
        }
    }
    /// Returns a reason when the connection is over.
    async fn on_client_event(
        &mut self,
        event: ClientEvent,
        handle: &Handle,
        events: &async_channel::Sender<AppEvent>,
    ) -> Option<String> {
        match event {
            ClientEvent::Connected => {
                show(events, Line::status("connected; registering")).await;
                None
            }
            ClientEvent::Session(event) => {
                match &event {
                    Event::CapsEnabled(caps) => {
                        self.echoed = caps.iter().any(|cap| cap == "echo-message");
                    }
                    Event::Call {
                        call_id,
                        verb,
                        from,
                        params,
                    } => {
                        if let Err(error) = self.calls.on_call_message(
                            handle,
                            call_id,
                            verb,
                            &from.nick,
                            from.account.as_deref(),
                            params,
                        ) {
                            show(events, Line::error(error.to_string())).await;
                        }
                    }
                    Event::LoggedIn { account } => {
                        self.calls
                            .set_account(Some(String::from_utf8_lossy(account).into_owned()));
                    }
                    Event::Registered { nick } => {
                        self.calls
                            .set_nick(String::from_utf8_lossy(nick).into_owned());
                    }
                    Event::NickChanged {
                        new, is_self: true, ..
                    } => {
                        self.calls
                            .set_nick(String::from_utf8_lossy(new).into_owned());
                    }
                    _ => {}
                }
                for app_event in self.translator.translate(event) {
                    let _ = events.send(app_event).await;
                }
                self.flush(events).await;
                None
            }
            // Shown only on request; a client that prints every raw line by
            // default is a protocol debugger, not a chat window.
            ClientEvent::RawIn(_) => None,
            ClientEvent::Disconnected(reason) => Some(reason),
        }
    }

    /// Returns a reason when the user has asked to leave.
    async fn on_ui_command(
        &mut self,
        command: UiCommand,
        handle: &Handle,
        events: &async_channel::Sender<AppEvent>,
    ) -> Option<String> {
        match command {
            UiCommand::Input { buffer, text } => {
                let actions = command::parse(&buffer, self.translator.me(), &text, self.echoed);
                for action in actions {
                    if let Some(reason) = apply(action, handle, events).await {
                        return Some(reason);
                    }
                }
                None
            }
            UiCommand::Call(action) => {
                let done = match action {
                    CallAction::Start(target) => self.calls.start(handle, &target),
                    CallAction::Answer => self.calls.answer(handle),
                    CallAction::Reject => self.calls.reject(handle),
                    CallAction::HangUp => self.calls.hang_up(handle),
                    CallAction::Verify => self.calls.verify(),
                };
                if let Err(error) = done {
                    show(events, Line::error(error.to_string())).await;
                }
                self.flush(events).await;
                None
            }
            UiCommand::SelfViewSink { sink } => {
                if let Err(error) = self.calls.attach_self_view(sink) {
                    show(events, Line::error(error.to_string())).await;
                }
                None
            }
            UiCommand::VideoSink { peer, sink } => {
                if let Err(error) = self.calls.attach_video_sink(&peer, sink) {
                    show(events, Line::error(error.to_string())).await;
                }
                None
            }
            UiCommand::Quit { reason } => {
                let _ = handle.quit(reason.clone());
                Some(reason)
            }
        }
    }
}

/// Carry out one parsed action. Returns a reason when it ends the session.
async fn apply(
    action: Action,
    handle: &Handle,
    events: &async_channel::Sender<AppEvent>,
) -> Option<String> {
    match action {
        Action::Send(message) => {
            if let Err(error) = handle.send(*message) {
                show(events, Line::error(format!("could not send: {error:#}"))).await;
            }
            None
        }
        Action::Show(buffer, line) => {
            let _ = events.send(AppEvent::Line { buffer, line }).await;
            None
        }
        Action::Open(buffer) => {
            let _ = events.send(AppEvent::OpenBuffer { buffer }).await;
            None
        }
        Action::Quit(reason) => {
            let _ = handle.quit(reason.clone());
            Some(reason)
        }
    }
}

/// How a notice from the call machinery should read.
fn line_of(notice: &Notice) -> Line {
    let kind = match notice.level {
        Level::Info => LineKind::Status,
        Level::Warning => LineKind::Error,
        // Not a shout for its own sake: an incoming call and the phrase two
        // people read to each other are both useless if they scroll past.
        Level::Highlight => LineKind::Highlight,
    };
    Line {
        kind,
        who: None,
        text: notice.text.clone(),
    }
}

/// Put a line in the server buffer.
async fn show(events: &async_channel::Sender<AppEvent>, line: Line) {
    let _ = events
        .send(AppEvent::Line {
            buffer: SERVER_BUFFER.to_owned(),
            line,
        })
        .await;
}

/// Open a connection and hand back both ends of it.
///
/// The pair is what an interface needs: somewhere to send what the user does,
/// and somewhere to read what happened. Creating them together means a caller
/// cannot wire one up and forget the other.
pub fn start(
    connect: ConnectConfig,
    session: SessionConfig,
    options: CallOptions,
) -> Result<(
    mpsc::UnboundedSender<UiCommand>,
    async_channel::Receiver<AppEvent>,
)> {
    let (events_tx, events_rx) = async_channel::unbounded();
    let commands = spawn(connect, session, options, events_tx)?;
    Ok((commands, events_rx))
}

/// Start the tokio half on a thread of its own.
///
/// GTK owns the process's main thread and its main loop, so the runtime cannot
/// live there: blocking on it would freeze the interface, and `#[tokio::main]`
/// would take the thread GTK needs.
///
/// Returns the sender the interface uses to talk to the connection.
pub fn spawn(
    connect: ConnectConfig,
    session: SessionConfig,
    options: CallOptions,
    events: async_channel::Sender<AppEvent>,
) -> Result<mpsc::UnboundedSender<UiCommand>> {
    let (tx, rx) = mpsc::unbounded_channel();

    std::thread::Builder::new()
        .name("kestrel-net".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    // Reported through the same channel as everything else, so
                    // the window shows a reason rather than staying blank.
                    let _ = events.send_blocking(AppEvent::Disconnected {
                        reason: format!("could not start the runtime: {error}"),
                    });
                    return;
                }
            };
            runtime.block_on(run(connect, session, options, rx, events));
        })?;

    Ok(tx)
}
