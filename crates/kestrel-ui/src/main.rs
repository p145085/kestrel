//! Kestrel's graphical client.
//!
//! Three things have to share one process and none of them will yield the main
//! thread politely, so the order in `main` matters: GTK owns the main thread
//! and its loop, the tokio runtime gets a thread of its own, and the two talk
//! only through channels. See `connection::spawn` for why.

// No console window in a release build -- a chat client should not open
// one. Debug builds keep it, because --help and anything logged would
// otherwise go nowhere on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod window;

use anyhow::{Context, Result, bail};
use gtk::glib;
use gtk::prelude::*;
use kestrel_net::ConnectConfig;
use kestrel_session::SessionConfig;
use kestrel_session::config::Sasl;

const USAGE: &str = "\
kestrel-ui — Kestrel's graphical IRC client

USAGE:
    kestrel-ui <host>[:<port>] [options]

OPTIONS:
    -n, --nick <nick>    Nickname to use
    -r, --real <name>    Real name
    -j, --join <chans>   Comma-separated channels to join on connect
    -p, --pass <pass>    Server password
        --sasl <account> Authenticate as this account
        --sasl-pass <p>  Password for SASL
        --tls            Connect with TLS (default port 6697)
        --insecure       With --tls, accept any certificate
    -h, --help           Show this
";

/// Where the interface will connect, and as whom.
struct Options {
    connect: ConnectConfig,
    session: SessionConfig,
}

fn main() -> Result<()> {
    // Warnings and worse only: this is a chat window, and its console is for
    // things that went wrong rather than a running commentary.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();

    let Some(options) = parse_args()? else {
        return Ok(());
    };

    // Unbounded from the interface, because `send` on one never blocks and so
    // is safe to call straight from a GTK signal handler.
    let (events_tx, events_rx) = async_channel::unbounded();
    let commands = kestrel_ui::connection::spawn(options.connect, options.session, events_tx)?;

    let app = gtk::Application::builder()
        .application_id("chat.kestrel.Client")
        // Arguments are ours, not GTK's, and it would otherwise refuse them.
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_activate(move |app| {
        let (ui, window) = window::Window::build(app, commands.clone());
        window::pump(ui.clone(), events_rx.clone());

        // Leaving properly rather than dropping the socket, so the server and
        // everyone in the channel see a reason rather than a timeout.
        window.connect_close_request(move |_| {
            ui.quit();
            glib::Propagation::Proceed
        });
    });

    // Emptied deliberately: GTK would otherwise try to parse our arguments and
    // exit complaining about the ones it does not recognise.
    app.run_with_args::<&str>(&[]);
    Ok(())
}

fn parse_args() -> Result<Option<Options>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(None);
    }

    let mut host = None;
    let mut port = None;
    let mut tls = false;
    let mut insecure = false;
    let mut nick = None;
    let mut realname = None;
    let mut join: Vec<String> = Vec::new();
    let mut server_password = None;
    let mut sasl_account = None;
    let mut sasl_password = None;

    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let mut next = |what: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .with_context(|| format!("{what} needs a value"))
        };
        match arg.as_str() {
            "--tls" => tls = true,
            "--insecure" => insecure = true,
            "-n" | "--nick" => nick = Some(next("--nick")?),
            "-r" | "--real" => realname = Some(next("--real")?),
            "-j" | "--join" => join = next("--join")?.split(',').map(str::to_owned).collect(),
            "-p" | "--pass" => server_password = Some(next("--pass")?),
            "--sasl" => sasl_account = Some(next("--sasl")?),
            "--sasl-pass" => sasl_password = Some(next("--sasl-pass")?),
            other if other.starts_with('-') => bail!("unknown option {other}"),
            other => {
                let (h, p) = match other.rsplit_once(':') {
                    Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
                        (h.to_owned(), Some(p.parse().context("invalid port")?))
                    }
                    _ => (other.to_owned(), None),
                };
                host = Some(h);
                port = p;
            }
        }
        index += 1;
    }

    let host = host.context("a server to connect to is required")?;
    let port = port.unwrap_or(if tls { 6697 } else { 6667 });

    let mut connect = if tls {
        ConnectConfig::tls(host, port)
    } else {
        ConnectConfig::plain(host, port)
    };
    connect.danger_accept_invalid_certs = insecure;

    let nick = nick.unwrap_or_else(|| "kestrel".to_owned());
    let mut session = SessionConfig::new(nick.clone());
    if let Some(realname) = realname {
        session.realname = realname.into_bytes();
    }
    session.autojoin = join.into_iter().map(String::into_bytes).collect();
    session.server_password = server_password.map(String::into_bytes);
    if let Some(account) = sasl_account {
        let password = sasl_password.context("--sasl needs --sasl-pass")?;
        session.sasl = Some(Sasl::Plain {
            account: account.into_bytes(),
            password: password.into_bytes(),
        });
    }

    Ok(Some(Options { connect, session }))
}
