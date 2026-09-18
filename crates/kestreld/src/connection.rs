//! Per-connection I/O: framing lines and pumping them to and from the core.
//!
//! Generic over the stream so that plaintext and TLS connections travel the
//! same path. Framing bugs are the kind that only show up under one transport,
//! and having two copies of this loop is how that happens.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use kestreld_core::ClientId;

/// Largest line accepted, in bytes.
///
/// The message portion is capped at 512 bytes and the tag section at 8191, so
/// anything beyond their sum is not a message we could ever have produced. A
/// client sending one is either broken or probing, and in both cases reading
/// it into memory is the wrong response.
pub const MAX_LINE: usize = kestrel_proto::limits::MAX_TAGS_TOTAL + kestrel_proto::limits::MAX_LINE;

/// How many lines may be queued for a slow client before it is disconnected.
///
/// Without a bound, one client that stops reading makes the server buffer
/// everything the channels it is in produce — a denial of service anyone can
/// trigger by connecting and going quiet.
pub const MAX_SEND_QUEUE: usize = 1024;

/// What a connection reports to the core.
#[derive(Debug)]
pub enum Event {
    /// A connection was accepted and wants an id.
    Connected {
        /// Hostname to show in the client's mask.
        host: Vec<u8>,
        /// SHA-256 fingerprint of the client's TLS certificate, if it gave one.
        certificate_fingerprint: Option<String>,
        /// Where to deliver this client's outbound lines.
        outbound: mpsc::Sender<Vec<u8>>,
        /// Where to send the assigned id.
        reply: oneshot::Sender<ClientId>,
    },
    /// A complete line arrived.
    Line(ClientId, Vec<u8>),
    /// The connection ended.
    Disconnected(ClientId, String),
}

/// Serve one accepted connection until it ends.
pub async fn serve<S>(
    stream: S,
    peer: SocketAddr,
    certificate_fingerprint: Option<String>,
    events: mpsc::Sender<Event>,
    idle_timeout: Duration,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(MAX_SEND_QUEUE);
    let (id_tx, id_rx) = oneshot::channel();

    if events
        .send(Event::Connected {
            host: peer.ip().to_string().into_bytes(),
            certificate_fingerprint,
            outbound: outbound_tx,
            reply: id_tx,
        })
        .await
        .is_err()
    {
        return; // The core is shutting down.
    }
    let Ok(id) = id_rx.await else {
        return;
    };

    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    // Writing runs in its own task so that a slow reader cannot stall the read
    // side, and vice versa.
    let writer = tokio::spawn(async move {
        while let Some(line) = outbound_rx.recv().await {
            if write_half.write_all(&line).await.is_err() {
                break;
            }
        }
        let _ = write_half.shutdown().await;
    });

    let reason = read_loop(&mut reader, id, &events, idle_timeout).await;

    writer.abort();
    let _ = events.send(Event::Disconnected(id, reason)).await;
}

async fn read_loop<S>(
    reader: &mut BufReader<ReadHalf<S>>,
    id: ClientId,
    events: &mpsc::Sender<Event>,
    idle_timeout: Duration,
) -> String
where
    S: AsyncRead + AsyncWrite,
{
    let mut line = Vec::with_capacity(512);

    loop {
        line.clear();
        let read = tokio::time::timeout(idle_timeout, reader.read_until(b'\n', &mut line)).await;

        // The timeout elapsing means the client has said nothing at all.
        let Ok(read) = read else {
            return "Ping timeout".to_owned();
        };

        match read {
            Ok(0) => return "Connection closed".to_owned(),
            Ok(_) => {}
            Err(error) => return format!("Read error: {error}"),
        }

        // `read_until` keeps the delimiter; strip the terminator here so the
        // parser never has to care which of CR, LF or both a client sent.
        while matches!(line.last(), Some(b'\r' | b'\n')) {
            line.pop();
        }

        if line.len() > MAX_LINE {
            warn!(%id, length = line.len(), "line exceeds the protocol limit");
            return "Excess flood".to_owned();
        }
        if line.is_empty() {
            continue; // Empty lines are legal and mean nothing.
        }

        if events.send(Event::Line(id, line.clone())).await.is_err() {
            return "Server shutting down".to_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MAX_LINE;

    #[test]
    fn the_line_cap_covers_both_budgets() {
        // Tags and the message portion are budgeted separately, so the cap has
        // to admit a message that legitimately uses all of both.
        assert_eq!(MAX_LINE, 8191 + 512);
    }
}
