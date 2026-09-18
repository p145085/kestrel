//! A terminal IRC client.
//!
//! Deliberately line-based rather than a full-screen interface: it exists to
//! exercise `kestrel-session` against a real server, and every behaviour it
//! shows is the same code the graphical client will use.

mod render;
mod ui;

use anyhow::{Context, Result, bail};
use kestrel_net::{ClientEvent, ConnectConfig};
use kestrel_session::{Sasl, SessionConfig};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

const USAGE: &str = "\
kestrel — a terminal IRC client

USAGE:
    kestrel <host>[:port] [OPTIONS]

OPTIONS:
    -n, --nick <NICK>        Nickname (default: your username)
    -r, --real <NAME>        Realname
    -j, --join <CHANNELS>    Comma-separated channels to join
    -p, --pass <PASSWORD>    Server password
        --sasl <ACCOUNT>     Authenticate as ACCOUNT; asks for the password
        --sasl-pass <PASS>   SASL password (avoid: visible in your shell history)
        --tls                Connect with TLS (default port 6697)
        --insecure           With --tls, skip certificate checking
    -h, --help               Show this

COMMANDS once connected:
    /join #chan              /part [#chan] [reason]
    /msg <target> <text>     /me <action>
    /nick <nick>             /topic [#chan] [topic]
    /names [#chan]           /whois <nick>
    /t <target>              Switch where plain text goes
    /raw <line>              Send a raw protocol line
    /quit [reason]
    Anything else is sent to the current target.";

/// Options gathered from the command line.
struct Options {
    connect: ConnectConfig,
    session: SessionConfig,
    join: Vec<String>,
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
                // host, or host:port
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

    let host = host.context("no server given; try --help")?;
    let port = port.unwrap_or(if tls { 6697 } else { 6667 });

    let nick = nick
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "kestrel".to_owned());

    let mut connect = if tls {
        ConnectConfig::tls(host, port)
    } else {
        ConnectConfig::plain(host, port)
    };
    connect.danger_accept_invalid_certs = insecure;

    let mut session = SessionConfig::new(nick.clone())
        .with_realname(realname.unwrap_or_else(|| nick.clone()))
        .with_autojoin(join.iter().map(String::as_bytes).collect::<Vec<_>>());
    session.server_password = server_password.map(String::into_bytes);

    if let Some(account) = sasl_account {
        let password = match sasl_password {
            Some(password) => password,
            None => ui::prompt_password(&format!("SASL password for {account}: "))?,
        };
        session = session.with_sasl(Sasl::Plain {
            account: account.into_bytes(),
            password: password.into_bytes(),
        });
    }

    Ok(Some(Options {
        connect,
        session,
        join,
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let Some(options) = parse_args()? else {
        return Ok(());
    };

    if options.connect.tls && options.connect.danger_accept_invalid_certs {
        ui::warn("certificate checking is off: you cannot tell who you are talking to");
    } else if !options.connect.tls {
        ui::warn("this connection is not encrypted");
    }
    ui::status(&format!(
        "connecting to {}:{}{}",
        options.connect.host,
        options.connect.port,
        if options.connect.tls { " over TLS" } else { "" }
    ));

    let (handle, mut events) = kestrel_net::connect(options.connect, options.session).await?;

    // Reading stdin blocks, so it gets its own task rather than stalling the
    // loop that has to keep answering PINGs.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if input_tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut state = ui::State::new(options.join.first().cloned());

    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break };
                if let ClientEvent::Disconnected(reason) = &event {
                    ui::status(&format!("disconnected: {reason}"));
                    break;
                }
                let was_registered = state.registered;
                render::show(&event, &mut state);
                // Anything typed while connecting runs now, in the order it
                // was typed.
                if state.registered && !was_registered {
                    for line in state.take_pending() {
                        if let Err(error) = ui::handle_input(&line, &handle, &mut state) {
                            ui::warn(&error.to_string());
                        }
                    }
                }
            }
            line = input_rx.recv() => {
                let Some(line) = line else {
                    let _ = handle.quit("Leaving");
                    continue;
                };
                if state.registered {
                    if let Err(error) = ui::handle_input(&line, &handle, &mut state) {
                        ui::warn(&error.to_string());
                    }
                } else {
                    state.defer(&line);
                }
            }
            () = wait_for_interrupt() => {
                ui::status("interrupted; quitting");
                let _ = handle.quit("Interrupted");
            }
        }
    }
    Ok(())
}

async fn wait_for_interrupt() {
    let _ = tokio::signal::ctrl_c().await;
}
