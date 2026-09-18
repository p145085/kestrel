//! A terminal IRC client.
//!
//! Deliberately line-based rather than a full-screen interface: it exists to
//! exercise `kestrel-session` against a real server, and every behaviour it
//! shows is the same code the graphical client will use.

mod calls;
mod render;
mod ui;

use anyhow::{Context, Result, bail};
use kestrel_net::{ClientEvent, ConnectConfig};
use kestrel_session::Event;
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
        --test-media         Use test tones instead of your microphone and camera
        --camera <name>      Use the camera whose name contains <name>
        --list-cameras       List the cameras this machine offers, and exit
        --audio-only         Do not offer video
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
    Anything else is sent to the current target.

CALLS:
    /call <nick|#chan>       Place a call
    /answer                  Answer a ringing call
    /reject                  Refuse it
    /hangup                  Leave the call
    /verify                  Confirm the spoken phrase matched
    /relayonly [on|off]      Hide your IP address from peers (needs a relay)";

/// Options gathered from the command line.
struct Options {
    connect: ConnectConfig,
    session: SessionConfig,
    join: Vec<String>,
    test_media: bool,
    audio_only: bool,
    camera: Option<String>,
}

fn parse_args() -> Result<Option<Options>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(None);
    }

    if args.iter().any(|a| a == "--list-cameras") {
        kestrel_media::init().context("GStreamer would not start")?;
        // In the order the system ranks them, because the first is what a
        // call takes when no camera is named.
        for (position, name) in kestrel_media::cameras().iter().enumerate() {
            let note = if position == 0 { "  (default)" } else { "" };
            println!("{name}{note}");
        }
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
    let mut test_media = false;
    let mut camera = None;
    let mut audio_only = false;

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
            "--test-media" => test_media = true,
            "--camera" => camera = Some(next("--camera")?),
            "--audio-only" => audio_only = true,
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
        test_media,
        audio_only,
        camera,
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

    // Media runs on GStreamer's own threads; everything it reports is funnelled
    // here so that calls are driven from the same loop as everything else.
    let (media_tx, mut media_rx) = mpsc::unbounded_channel();
    if let Err(error) = kestrel_media::init() {
        ui::warn(&format!("calls are unavailable: {error}"));
    }
    let mut calls = calls::Calls::new(media_tx);
    if options.test_media {
        calls.use_test_media();
    }
    if let Some(camera) = options.camera {
        calls.use_camera(camera);
    }
    if options.audio_only {
        calls.audio_only();
    }

    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break };
                if let ClientEvent::Disconnected(reason) = &event {
                    ui::status(&format!("disconnected: {reason}"));
                    break;
                }
                // Calls are handled before rendering, so a ringing call is
                // acted on rather than merely printed.
                if let ClientEvent::Session(session_event) = &event {
                    match session_event {
                        Event::Call { call_id, verb, from, params } => {
                            if let Err(error) = calls.on_call_message(
                                &handle, call_id, verb, &from.nick, params,
                            ) {
                                ui::warn(&error.to_string());
                            }
                        }
                        Event::LoggedIn { account } => {
                            calls.set_account(Some(String::from_utf8_lossy(account).into_owned()));
                        }
                        Event::Registered { nick } => {
                            calls.set_nick(String::from_utf8_lossy(nick).into_owned());
                        }
                        Event::NickChanged { new, is_self: true, .. } => {
                            calls.set_nick(String::from_utf8_lossy(new).into_owned());
                        }
                        _ => {}
                    }
                }

                let was_registered = state.registered;
                render::show(&event, &mut state);
                // Anything typed while connecting runs now, in the order it
                // was typed.
                if state.registered && !was_registered {
                    for line in state.take_pending() {
                        if let Err(error) =
                            ui::handle_input(&line, &handle, &mut state, &mut calls)
                        {
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
                    if let Err(error) =
                        ui::handle_input(&line, &handle, &mut state, &mut calls)
                    {
                        ui::warn(&error.to_string());
                    }
                } else {
                    state.defer(&line);
                }
            }
            Some(media) = media_rx.recv() => {
                if let Err(error) = calls.on_media(&handle, media) {
                    ui::warn(&error.to_string());
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
