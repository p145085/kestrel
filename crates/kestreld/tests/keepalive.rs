//! A quiet connection is asked before it is given up on.
//!
//! Silence is not absence. Two people who leave a client open and say nothing
//! for an afternoon are still there, and a server that drops them for it has
//! invented a disconnection out of nothing.

use std::time::Duration;

use kestreld::connection::{Event, serve};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Run one connection, and hand back the client's end of the wire.
///
/// The core is not involved: what matters here is what reaches the socket and
/// when, which the read loop decides on its own.
fn connect(idle_timeout: Duration) -> (tokio::io::DuplexStream, mpsc::Receiver<Event>) {
    let (client, server) = tokio::io::duplex(4096);
    let (events_tx, mut events_rx) = mpsc::channel(16);
    let (forward_tx, forward_rx) = mpsc::channel(16);

    // Stand in for the core, which only has to answer with an identifier
    // before the connection will start reading.
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            if let Event::Connected { reply, .. } = event {
                let _ = reply.send(kestreld_core::ClientId::from_raw(1));
            } else if forward_tx.send(event).await.is_err() {
                break;
            }
        }
    });

    tokio::spawn(serve(
        server,
        "127.0.0.1:0".parse().expect("an address"),
        None,
        events_tx,
        idle_timeout,
    ));

    (client, forward_rx)
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_silent_client_is_asked_rather_than_dropped() {
    let (client, mut events) = connect(Duration::from_mins(1));
    let mut lines = BufReader::new(client).lines();

    // Nothing is said for long enough that a server without a keepalive would
    // already have given up.
    let asked = tokio::time::timeout(Duration::from_secs(45), lines.next_line())
        .await
        .expect("the server should ask within the idle window")
        .expect("reading should succeed")
        .expect("a line should arrive");

    assert!(asked.starts_with("PING"), "got {asked:?}");
    assert!(
        events.try_recv().is_err(),
        "being quiet is not a disconnection"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn answering_keeps_the_connection() {
    let (client, mut events) = connect(Duration::from_mins(1));
    let (read, mut write) = tokio::io::split(client);
    let mut lines = BufReader::new(read).lines();

    let asked = tokio::time::timeout(Duration::from_secs(45), lines.next_line())
        .await
        .expect("the server should ask")
        .expect("reading should succeed")
        .expect("a line should arrive");
    assert!(asked.starts_with("PING"));

    write
        .write_all(b"PONG :keepalive\r\n")
        .await
        .expect("writing should succeed");

    // Past the point where an unanswered question would have ended it. The
    // server should be asking again rather than hanging up.
    let again = tokio::time::timeout(Duration::from_mins(2), lines.next_line())
        .await
        .expect("the server should ask a second time")
        .expect("reading should succeed")
        .expect("a line should arrive");
    assert!(again.starts_with("PING"), "got {again:?}");

    let ended = events.try_recv();
    assert!(
        !matches!(ended, Ok(Event::Disconnected(..))),
        "a client that answered was dropped anyway: {ended:?}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ignoring_the_question_ends_the_connection() {
    let (client, mut events) = connect(Duration::from_mins(1));
    let mut lines = BufReader::new(client).lines();

    // Read the question, then say nothing at all.
    let _ = tokio::time::timeout(Duration::from_secs(45), lines.next_line()).await;

    let ended = tokio::time::timeout(Duration::from_mins(2), events.recv())
        .await
        .expect("the connection should end");

    match ended {
        Some(Event::Disconnected(_, reason)) => {
            assert_eq!(reason, "Ping timeout", "and it should say why");
        }
        other => panic!("expected a disconnection, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_line_split_across_the_question_still_arrives_whole() {
    // The read is cancelled when the timer fires, and it can be cancelled
    // partway through a line. Those bytes are the start of a real message, so
    // losing them would corrupt whatever the client was saying.
    let (client, mut events) = connect(Duration::from_mins(1));
    let (read, mut write) = tokio::io::split(client);
    let mut lines = BufReader::new(read).lines();

    write.write_all(b"JOIN #hal").await.expect("first half");

    let asked = tokio::time::timeout(Duration::from_secs(45), lines.next_line())
        .await
        .expect("the server should still ask")
        .expect("reading should succeed")
        .expect("a line should arrive");
    assert!(asked.starts_with("PING"));

    write.write_all(b"f\r\n").await.expect("second half");

    let line = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("the line should arrive");

    match line {
        Some(Event::Line(_, bytes)) => assert_eq!(
            String::from_utf8_lossy(&bytes),
            "JOIN #half",
            "the halves either side of the question must join up"
        ),
        other => panic!("expected the line, got {other:?}"),
    }
}
