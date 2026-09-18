//! The half of the client that owns the socket.
//!
//! Runs on the tokio thread and never touches a widget. Everything it wants
//! shown leaves as an [`AppEvent`]; everything the user does arrives as a
//! [`UiCommand`]. That one-way split is what keeps GTK's `!Send` types and
//! tokio's work on opposite sides of a thread boundary without any locking.

use anyhow::Result;
use kestrel_net::{ClientEvent, ConnectConfig, Handle};
use kestrel_session::SessionConfig;
use kestrel_session::event::Event;
use tokio::sync::mpsc;

use crate::command::{self, Action};
use crate::event::{AppEvent, Line, SERVER_BUFFER, UiCommand};
use crate::translate::Translator;

/// Drive one connection until it ends.
///
/// Returns once the server is gone or the user has quit; the interface learns
/// which from the final [`AppEvent::Disconnected`].
pub async fn run(
    connect: ConnectConfig,
    session: SessionConfig,
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

    let mut state = State::default();
    let reason = loop {
        tokio::select! {
            event = incoming.recv() => {
                // The connection task ended without telling us why, which is
                // itself worth saying rather than closing in silence.
                let Some(event) = event else {
                    break "connection closed".to_owned();
                };
                if let Some(reason) = state.on_client_event(event, &events).await {
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
        }
    };

    let _ = events.send(AppEvent::Disconnected { reason }).await;
}

/// What the connection half has to remember.
#[derive(Default)]
struct State {
    translator: Translator,
    /// Whether the server sends our own messages back to us.
    ///
    /// Decides whether saying something should also be shown locally. Getting
    /// this wrong in either direction is visible immediately: every message
    /// appears twice, or our own never appear at all.
    echoed: bool,
}

impl State {
    /// Returns a reason when the connection is over.
    async fn on_client_event(
        &mut self,
        event: ClientEvent,
        events: &async_channel::Sender<AppEvent>,
    ) -> Option<String> {
        match event {
            ClientEvent::Connected => {
                show(events, Line::status("connected; registering")).await;
                None
            }
            ClientEvent::Session(event) => {
                if let Event::CapsEnabled(caps) = &event {
                    self.echoed = caps.iter().any(|cap| cap == "echo-message");
                }
                for app_event in self.translator.translate(event) {
                    let _ = events.send(app_event).await;
                }
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
) -> Result<(
    mpsc::UnboundedSender<UiCommand>,
    async_channel::Receiver<AppEvent>,
)> {
    let (events_tx, events_rx) = async_channel::unbounded();
    let commands = spawn(connect, session, events_tx)?;
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
            runtime.block_on(run(connect, session, rx, events));
        })?;

    Ok(tx)
}
