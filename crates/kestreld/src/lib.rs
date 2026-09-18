//! The Kestrel IRC server's runtime.
//!
//! All protocol behaviour lives in `kestreld-core`, which is sans-io. This
//! crate is the part that owns sockets and the clock: it accepts connections,
//! frames lines, and drives the state machine. Keeping the split sharp means a
//! bug here is a networking bug, and a bug there is a protocol bug, and the
//! two are never confused for one another.

pub mod config;
pub mod connection;

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use kestrel_proto::Message;
use kestreld_core::{Action, ClientId, Server};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::connection::Event;

/// How many events may be queued from connections before backpressure applies.
const EVENT_QUEUE: usize = 4096;

/// The current time in Unix seconds.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Bind every address the configuration asks for.
///
/// Binding is separate from serving so that a caller — a test, or a future
/// socket-activation path — can supply listeners it already holds, and so that
/// a failure to bind is reported before any connection is accepted.
pub async fn bind(config: &Config) -> Result<Vec<TcpListener>> {
    let mut listeners = Vec::with_capacity(config.listen.len());
    for address in &config.listen {
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("binding {address}"))?;
        listeners.push(listener);
    }
    Ok(listeners)
}

/// Build the server state machine from configuration.
fn build_server(config: &Config) -> Server {
    let mut server = Server::new(config.to_server_config(), now_unix());
    for account in &config.accounts {
        match server.accounts_mut().register(
            account.name.as_bytes(),
            account.password.as_bytes(),
            now_unix(),
        ) {
            Ok(()) => info!(name = %account.name, "registered account from configuration"),
            Err(error) => warn!(name = %account.name, %error, "could not register account"),
        }
    }
    server
}

/// Run the server, binding from configuration and stopping on Ctrl-C.
pub async fn run(config: Config) -> Result<()> {
    let listeners = bind(&config).await?;
    for listener in &listeners {
        if let Ok(address) = listener.local_addr() {
            info!(%address, "listening");
        }
    }
    serve(config, listeners, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// Serve on already-bound listeners until `shutdown` completes.
pub async fn serve<S>(config: Config, listeners: Vec<TcpListener>, shutdown: S) -> Result<()>
where
    S: Future<Output = ()> + Send,
{
    let mut server = build_server(&config);
    let (events_tx, mut events_rx) = mpsc::channel::<Event>(EVENT_QUEUE);
    let idle_timeout = Duration::from_secs(config.idle_timeout_secs);

    let mut accept_tasks = Vec::with_capacity(listeners.len());
    for listener in listeners {
        let events = events_tx.clone();
        accept_tasks.push(tokio::spawn(accept_loop(listener, events, idle_timeout)));
    }
    drop(events_tx);

    let mut outbound: HashMap<ClientId, mpsc::Sender<Vec<u8>>> = HashMap::new();
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        let event = tokio::select! {
            event = events_rx.recv() => event,
            () = &mut shutdown => {
                info!("shutting down");
                break;
            }
        };
        let Some(event) = event else { break };

        let mut actions = Vec::new();
        handle_event(&mut server, &mut outbound, event, &mut actions);
        dispatch(&actions, &mut outbound);
    }

    for task in accept_tasks {
        task.abort();
    }
    Ok(())
}

async fn accept_loop(listener: TcpListener, events: mpsc::Sender<Event>, idle_timeout: Duration) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let events = events.clone();
                tokio::spawn(connection::serve(stream, peer, events, idle_timeout));
            }
            Err(error) => {
                error!(%error, "accept failed");
                // A failed accept is usually transient — a descriptor limit, or
                // a connection reset mid-handshake. Pausing avoids spinning the
                // CPU if it turns out to be persistent.
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

fn handle_event(
    server: &mut Server,
    outbound: &mut HashMap<ClientId, mpsc::Sender<Vec<u8>>>,
    event: Event,
    actions: &mut Vec<Action>,
) {
    match event {
        Event::Connected {
            host,
            outbound: sender,
            reply,
        } => {
            let id = server.connect(host);
            outbound.insert(id, sender);
            debug!(%id, "connected");
            if reply.send(id).is_err() {
                // The connection went away between accept and assignment.
                server.disconnect(id, b"Connection closed", actions);
                outbound.remove(&id);
            }
        }
        Event::Line(id, line) => match Message::parse(&line) {
            Ok(message) => server.handle(id, &message, now_unix(), actions),
            // An unparseable line is ignored rather than fatal. Clients and
            // bouncers emit stray malformed lines, and dropping the connection
            // over one is worse than dropping the line.
            Err(error) => debug!(%id, %error, "ignoring unparseable line"),
        },
        Event::Disconnected(id, reason) => {
            debug!(%id, %reason, "disconnected");
            server.disconnect(id, reason.as_bytes(), actions);
            outbound.remove(&id);
        }
    }
}

/// Deliver the state machine's output to the connections it names.
fn dispatch(actions: &[Action], outbound: &mut HashMap<ClientId, mpsc::Sender<Vec<u8>>>) {
    for action in actions {
        match action {
            Action::Send { to, message } => {
                let Some(sender) = outbound.get(to) else {
                    continue;
                };
                let Ok(bytes) = message.to_vec() else {
                    // The state machine built a message that cannot be
                    // represented. That is a bug here, not bad input, so it
                    // deserves a loud log rather than a silent drop.
                    error!(to = %to, "could not serialise an outgoing message");
                    continue;
                };
                // A full queue means the client is not reading. Dropping it is
                // better than growing the buffer without limit.
                if sender.try_send(bytes).is_err() {
                    debug!(to = %to, "send queue full or closed; dropping client");
                    outbound.remove(to);
                }
            }
            Action::Close { client } => {
                outbound.remove(client);
            }
        }
    }
}

/// The addresses a set of listeners is bound to.
#[must_use]
pub fn bound_addresses(listeners: &[TcpListener]) -> Vec<SocketAddr> {
    listeners
        .iter()
        .filter_map(|l| l.local_addr().ok())
        .collect()
}
