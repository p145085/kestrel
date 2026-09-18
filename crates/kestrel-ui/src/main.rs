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

mod connect;
mod window;

use anyhow::{Context, Result, bail};
use gtk::prelude::*;
use kestrel_net::ConnectConfig;
use kestrel_session::SessionConfig;
use kestrel_session::config::Sasl;
use kestrel_ui::connection::CallOptions;

const USAGE: &str = "\
kestrel-ui — Kestrel's graphical IRC client

USAGE:
    kestrel-ui [<host>[:<port>]] [options]

    With no host, a connection window is shown instead.

OPTIONS:
    -n, --nick <nick>    Nickname to use
    -r, --real <name>    Real name
    -j, --join <chans>   Comma-separated channels to join on connect
    -p, --pass <pass>    Server password
        --sasl <account> Authenticate as this account
        --sasl-pass <p>  Password for SASL
        --test-media     Use test tones instead of your microphone and camera
        --camera <name>  Use the camera whose name contains <name>
        --audio-only     Do not offer video
        --tls            Connect with TLS (default port 6697)
        --insecure       With --tls, accept any certificate
    -h, --help           Show this
";

/// Where the interface will connect, and as whom.
struct Options {
    /// Absent when no server was named, which is what opens the dialog.
    connect: Option<ConnectConfig>,
    session: SessionConfig,
    calls: CallOptions,
}

fn main() -> Result<()> {
    // Before anything else. A graphical build has no console, so without this
    // a panic prints to nowhere and the window simply vanishes -- which is
    // indistinguishable from somebody closing it.
    kestrel_client::crash::write_panics_to_a_file();

    // To a file as well as the console. A crash that is not a Rust panic
    // leaves no report, and what the pipeline was doing just before is then
    // the only evidence there is. Set KESTREL_LOG=debug for more of it.
    let level = match std::env::var("KESTREL_LOG").as_deref() {
        Ok("debug") => tracing::Level::DEBUG,
        Ok("info") => tracing::Level::INFO,
        Ok("trace") => tracing::Level::TRACE,
        _ => tracing::Level::WARN,
    };
    match kestrel_client::crash::log_file().and_then(open_log) {
        Some(file) => tracing_subscriber::fmt()
            .with_max_level(level)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => tracing_subscriber::fmt().with_max_level(level).init(),
    }

    let Some(options) = parse_args()? else {
        return Ok(());
    };

    if let Some(log) = kestrel_client::crash::crash_log()
        && log.exists()
    {
        // Said once at startup rather than never: a crash report nobody knows
        // about is the same as no crash report.
        eprintln!("a previous run left a crash report at {}", log.display());
    }

    let app = gtk::Application::builder()
        .application_id("chat.kestrel.Client")
        // Arguments are ours, not GTK's, and it would otherwise refuse them.
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    // Taken on the first activation: a connection is opened once, and what
    // follows is opened from the window's own menu.
    let options = std::cell::RefCell::new(Some(options));
    app.connect_activate(move |app| match options.borrow_mut().take() {
        // Told where to go, so go there.
        Some(Options {
            connect: Some(connect),
            session,
            calls,
        }) => {
            if let Err(error) = window::open(app, connect, session, calls) {
                eprintln!("could not start: {error:#}");
            }
        }
        // Nothing on the command line, so ask.
        Some(Options {
            connect: None,
            calls,
            ..
        }) => connect::show(app, None, &calls),
        None => connect::show(app, None, &CallOptions::default()),
    });

    // Emptied deliberately: GTK would otherwise try to parse our arguments and
    // exit complaining about the ones it does not recognise.
    app.run_with_args::<&str>(&[]);
    Ok(())
}

/// Open the log for appending, making its directory if need be.
fn open_log(path: std::path::PathBuf) -> Option<std::fs::File> {
    if let Some(directory) = path.parent() {
        let _ = std::fs::create_dir_all(directory);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

fn parse_args() -> Result<Option<Options>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
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
    let mut calls = CallOptions::default();

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
            "--test-media" => calls.test_media = true,
            "--audio-only" => calls.audio_only = true,
            "--camera" => calls.camera = Some(next("--camera")?),
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

    let connect = host.map(|host| {
        let port = port.unwrap_or(if tls { 6697 } else { 6667 });
        let mut connect = if tls {
            ConnectConfig::tls(host, port)
        } else {
            ConnectConfig::plain(host, port)
        };
        connect.danger_accept_invalid_certs = insecure;
        connect
    });

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

    Ok(Some(Options {
        connect,
        session,
        calls,
    }))
}
