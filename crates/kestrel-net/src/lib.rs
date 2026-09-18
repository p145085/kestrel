//! The client transport: sockets, TLS, and the task that drives a session.
//!
//! `kestrel-session` decides *what* to say; this decides how bytes get there.
//! Keeping the split sharp means a bug here is a networking bug and a bug
//! there is a protocol bug, and the two are never confused for one another.

mod tls;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use kestrel_proto::{Message, MessageBuf};
use kestrel_session::{Action, Event, Session, SessionConfig};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::debug;

pub use tls::danger_accept_any_certificate;

/// Largest line accepted from a server.
///
/// The message portion is capped at 512 bytes and the tag section at 8191, so
/// anything longer is not something a conforming server could have sent.
pub const MAX_LINE: usize = kestrel_proto::limits::MAX_TAGS_TOTAL + kestrel_proto::limits::MAX_LINE;

/// Where and how to connect.
#[derive(Debug, Clone)]
pub struct ConnectConfig {
    /// Hostname or address.
    pub host: String,
    /// Port.
    pub port: u16,
    /// Whether to wrap the connection in TLS.
    pub tls: bool,
    /// Skip certificate validation.
    ///
    /// Only for a server whose self-signed certificate you have checked by
    /// other means. It removes the guarantee that you are talking to the
    /// server you named rather than to whoever answered.
    pub danger_accept_invalid_certs: bool,
    /// How long to wait for a server that has gone quiet.
    pub idle_timeout: Duration,
}

impl ConnectConfig {
    /// A plaintext connection to `host:port`.
    #[must_use]
    pub fn plain(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            tls: false,
            danger_accept_invalid_certs: false,
            idle_timeout: Duration::from_mins(5),
        }
    }

    /// A TLS connection to `host:port`.
    #[must_use]
    pub fn tls(host: impl Into<String>, port: u16) -> Self {
        Self {
            tls: true,
            ..Self::plain(host, port)
        }
    }
}

/// What the connection reports upwards.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// The socket is open and registration has begun.
    Connected,
    /// Something the session recognised.
    Session(Event),
    /// A raw line, for a client that wants to show traffic.
    RawIn(String),
    /// The connection ended.
    Disconnected(String),
}

/// What a caller can ask the connection to do.
#[derive(Debug, Clone)]
pub enum Command {
    /// Send this message.
    Send(MessageBuf),
    /// Send a raw line, already framed without its terminator.
    SendRaw(String),
    /// Quit with a reason and close.
    Quit(String),
}

/// A handle for talking to a running connection.
#[derive(Debug, Clone)]
pub struct Handle {
    commands: mpsc::UnboundedSender<Command>,
}

impl Handle {
    /// Queue a message. Fails only once the connection has ended.
    pub fn send(&self, message: MessageBuf) -> Result<()> {
        self.commands
            .send(Command::Send(message))
            .context("connection closed")
    }

    /// Queue a raw line.
    pub fn send_raw(&self, line: impl Into<String>) -> Result<()> {
        self.commands
            .send(Command::SendRaw(line.into()))
            .context("connection closed")
    }

    /// Quit and close.
    pub fn quit(&self, reason: impl Into<String>) -> Result<()> {
        self.commands
            .send(Command::Quit(reason.into()))
            .context("connection closed")
    }
}

/// Connect and drive a session until it ends.
///
/// Returns a handle for sending and a receiver of everything that happens.
pub async fn connect(
    connect: ConnectConfig,
    session: SessionConfig,
) -> Result<(Handle, mpsc::UnboundedReceiver<ClientEvent>)> {
    let address = (connect.host.as_str(), connect.port);
    let stream = TcpStream::connect(address)
        .await
        .with_context(|| format!("connecting to {}:{}", connect.host, connect.port))?;
    // IRC is made of small writes; batching them adds latency for no benefit.
    let _ = stream.set_nodelay(true);

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    if connect.tls {
        let connector = tls::connector(connect.danger_accept_invalid_certs)?;
        let name = tls::server_name(&connect.host)?;
        let stream = connector
            .connect(name, stream)
            .await
            .context("TLS handshake failed")?;
        tokio::spawn(run(stream, session, connect, command_rx, event_tx));
    } else {
        tokio::spawn(run(stream, session, connect, command_rx, event_tx));
    }

    Ok((
        Handle {
            commands: command_tx,
        },
        event_rx,
    ))
}

/// Own the socket and the session, and keep them in step.
async fn run<S>(
    stream: S,
    session_config: SessionConfig,
    connect: ConnectConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<ClientEvent>,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut session = Session::new(session_config);

    let _ = events.send(ClientEvent::Connected);

    // Registration starts the moment the socket is up.
    let opening = session.start();
    if !write_outcome(&mut write_half, &opening.actions).await {
        let _ = events.send(ClientEvent::Disconnected("write failed".into()));
        return;
    }
    for event in opening.events {
        let _ = events.send(ClientEvent::Session(event));
    }

    let mut line = Vec::with_capacity(512);
    let reason = loop {
        line.clear();
        tokio::select! {
            read = tokio::time::timeout(connect.idle_timeout, reader.read_until(b'\n', &mut line)) => {
                let Ok(read) = read else {
                    break "server went quiet".to_owned();
                };
                match read {
                    Ok(0) => break "server closed the connection".to_owned(),
                    Ok(_) => {}
                    Err(error) => break format!("read error: {error}"),
                }

                while matches!(line.last(), Some(b'\r' | b'\n')) {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                if line.len() > MAX_LINE {
                    break "server sent an oversized line".to_owned();
                }

                let _ = events.send(ClientEvent::RawIn(String::from_utf8_lossy(&line).into_owned()));

                // An unparseable line is dropped rather than fatal: servers and
                // bouncers emit stray malformed lines, and losing the
                // connection over one is worse than losing the line.
                let Ok(message) = Message::parse(&line) else {
                    debug!("ignoring an unparseable line");
                    continue;
                };
                let outcome = session.handle(&message);
                if !write_outcome(&mut write_half, &outcome.actions).await {
                    break "write failed".to_owned();
                }
                let ended = outcome
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::Ended(_)));
                for event in outcome.events {
                    let _ = events.send(ClientEvent::Session(event));
                }
                if ended || outcome.actions.contains(&Action::Disconnect) {
                    break "session ended".to_owned();
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break "closed locally".to_owned();
                };
                match command {
                    Command::Send(message) => {
                        if !write_message(&mut write_half, &message).await {
                            break "write failed".to_owned();
                        }
                    }
                    Command::SendRaw(text) => {
                        let mut bytes = text.into_bytes();
                        bytes.extend_from_slice(b"\r\n");
                        if write_half.write_all(&bytes).await.is_err() {
                            break "write failed".to_owned();
                        }
                    }
                    Command::Quit(reason) => {
                        let quit = MessageBuf::new("QUIT").trailing(reason);
                        let _ = write_message(&mut write_half, &quit).await;
                        break "quit".to_owned();
                    }
                }
            }
        }
    };

    let _ = write_half.shutdown().await;
    let _ = events.send(ClientEvent::Disconnected(reason));
}

async fn write_outcome<W: AsyncWrite + Unpin>(writer: &mut W, actions: &[Action]) -> bool {
    for action in actions {
        if let Action::Send(message) = action
            && !write_message(writer, message).await
        {
            return false;
        }
    }
    true
}

async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, message: &MessageBuf) -> bool {
    let Ok(bytes) = message.to_vec() else {
        // The session built something unrepresentable. That is a bug here, not
        // bad input, so skip the line rather than dropping the connection.
        debug!("could not serialise an outgoing message");
        return true;
    };
    writer.write_all(&bytes).await.is_ok()
}

/// A shared `rustls` provider, installed once.
pub(crate) fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[cfg(test)]
mod tests {
    use super::{ConnectConfig, MAX_LINE};

    #[test]
    fn the_line_cap_covers_both_budgets() {
        assert_eq!(MAX_LINE, 8191 + 512);
    }

    #[test]
    fn plain_and_tls_configs_differ_only_in_tls() {
        let plain = ConnectConfig::plain("irc.example.org", 6667);
        let tls = ConnectConfig::tls("irc.example.org", 6697);
        assert!(!plain.tls);
        assert!(tls.tls);
        assert!(
            !tls.danger_accept_invalid_certs,
            "certificate checking must be on unless asked otherwise"
        );
    }
}
