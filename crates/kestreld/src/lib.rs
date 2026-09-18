//! The Kestrel IRC server's runtime.
//!
//! All protocol behaviour lives in `kestreld-core`, which is sans-io. This
//! crate is the part that owns sockets and the clock: it accepts connections,
//! frames lines, and drives the state machine. Keeping the split sharp means a
//! bug here is a networking bug, and a bug there is a protocol bug, and the
//! two are never confused for one another.

pub mod config;
pub mod connection;
pub mod persist;
pub mod tls;

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use kestrel_proto::Message;
use kestreld_core::{Action, ClientId, Server};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
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

/// Bind every TLS address the configuration asks for.
pub async fn bind_tls(config: &Config) -> Result<Vec<TcpListener>> {
    let mut listeners = Vec::with_capacity(config.tls_listen.len());
    for address in &config.tls_listen {
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("binding {address}"))?;
        listeners.push(listener);
    }
    Ok(listeners)
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

    // Saved accounts load first, so a config entry for an account that has
    // since changed its password does not quietly reset it.
    if let Some(path) = &config.accounts_file {
        persist::load_into(server.accounts_mut(), path);
    }
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
    let acceptor = match config.tls_paths()? {
        Some((certificate, key)) => Some(tls::acceptor(certificate, key)?),
        None => None,
    };

    let listeners = bind(&config).await?;
    let tls_listeners = bind_tls(&config).await?;
    for listener in &listeners {
        if let Ok(address) = listener.local_addr() {
            info!(%address, "listening");
        }
    }
    for listener in &tls_listeners {
        if let Ok(address) = listener.local_addr() {
            info!(%address, "listening (TLS)");
        }
    }
    if listeners.is_empty() && tls_listeners.is_empty() {
        anyhow::bail!("no listen or tls_listen addresses are configured");
    }

    serve_all(config, listeners, tls_listeners, acceptor, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// Serve on already-bound plaintext listeners until `shutdown` completes.
pub async fn serve<S>(config: Config, listeners: Vec<TcpListener>, shutdown: S) -> Result<()>
where
    S: Future<Output = ()> + Send,
{
    serve_all(config, listeners, Vec::new(), None, shutdown).await
}

/// Serve on plaintext and TLS listeners until `shutdown` completes.
pub async fn serve_all<S>(
    config: Config,
    listeners: Vec<TcpListener>,
    tls_listeners: Vec<TcpListener>,
    acceptor: Option<TlsAcceptor>,
    shutdown: S,
) -> Result<()>
where
    S: Future<Output = ()> + Send,
{
    serve_all_with(config, listeners, tls_listeners, acceptor, |_| {}, shutdown).await
}

/// As [`serve_all`], but running `prepare` against the freshly built server.
///
/// The server owns its own state, so anything that has to be seeded before the
/// first connection — certificate fingerprints, channels restored from disk —
/// needs a hook here rather than a handle handed out afterwards.
pub async fn serve_all_with<P, S>(
    config: Config,
    listeners: Vec<TcpListener>,
    tls_listeners: Vec<TcpListener>,
    acceptor: Option<TlsAcceptor>,
    prepare: P,
    shutdown: S,
) -> Result<()>
where
    P: FnOnce(&mut Server),
    S: Future<Output = ()> + Send,
{
    let mut server = build_server(&config);
    prepare(&mut server);
    let accounts_file = config.accounts_file.clone();
    let (events_tx, mut events_rx) = mpsc::channel::<Event>(EVENT_QUEUE);
    let idle_timeout = Duration::from_secs(config.idle_timeout_secs);

    let mut accept_tasks = Vec::with_capacity(listeners.len() + tls_listeners.len());
    for listener in listeners {
        let events = events_tx.clone();
        accept_tasks.push(tokio::spawn(accept_loop(listener, events, idle_timeout)));
    }
    if !tls_listeners.is_empty() && acceptor.is_none() {
        anyhow::bail!("TLS listeners were supplied without a TLS acceptor");
    }
    for listener in tls_listeners {
        let events = events_tx.clone();
        let Some(acceptor) = acceptor.clone() else {
            continue;
        };
        accept_tasks.push(tokio::spawn(tls_accept_loop(
            listener,
            acceptor,
            events,
            idle_timeout,
        )));
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

        // Writing happens off the event loop: the store is small, but a
        // slow disk must not stall every other client while it finishes.
        if server.take_accounts_changed()
            && let Some(path) = accounts_file.clone()
        {
            let snapshot: Vec<_> = server.accounts().iter().cloned().collect();
            tokio::task::spawn_blocking(move || {
                if let Err(error) = persist::save(&path, snapshot) {
                    error!(path = %path.display(), %error, "could not save accounts");
                }
            });
        }
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
                set_nodelay(&stream, peer);
                let events = events.clone();
                tokio::spawn(connection::serve(stream, peer, None, events, idle_timeout));
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

/// Accept TLS connections, completing each handshake off the accept path.
async fn tls_accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    events: mpsc::Sender<Event>,
    idle_timeout: Duration,
) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                set_nodelay(&stream, peer);
                let acceptor = acceptor.clone();
                let events = events.clone();
                // The handshake runs in the connection's own task: a client
                // that stalls mid-handshake must not hold up every other
                // connection waiting to be accepted.
                tokio::spawn(async move {
                    let Ok(stream) = acceptor.accept(stream).await else {
                        debug!(%peer, "TLS handshake failed");
                        return;
                    };
                    let fingerprint = stream
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(<[_]>::first)
                        .map(tls::fingerprint);
                    connection::serve(stream, peer, fingerprint, events, idle_timeout).await;
                });
            }
            Err(error) => {
                error!(%error, "accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

/// IRC is made of small writes, so batching them adds latency for no benefit.
fn set_nodelay(stream: &TcpStream, peer: SocketAddr) {
    if let Err(error) = stream.set_nodelay(true) {
        debug!(%peer, %error, "could not disable Nagle");
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
            certificate_fingerprint,
            outbound: sender,
            reply,
        } => {
            let id = server.connect(host);
            outbound.insert(id, sender);
            // The fingerprint comes from the TLS layer, never from the client,
            // which is the whole basis of authenticating by certificate.
            if let Some(fingerprint) = certificate_fingerprint {
                debug!(%id, %fingerprint, "client presented a certificate");
                server.set_certificate_fingerprint(id, fingerprint);
            }
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
