//! Drive the interface's half of a connection without opening a window.
//!
//! What the client shows is decided entirely by the events printed here, and
//! what it can do is decided by the commands sent back. Both ends of that are
//! exercised without a display, which is what makes the window's behaviour
//! testable at all.
//!
//!     cargo run -p kestrel-ui --example dump_events -- \
//!         127.0.0.1:6667 alice '#test' --test-media --call bob

use std::time::Duration;

use kestrel_net::ConnectConfig;
use kestrel_session::SessionConfig;
use kestrel_ui::connection::{self, CallOptions};
use kestrel_ui::event::{AppEvent, CallAction, UiCommand};

struct Args {
    address: String,
    nick: String,
    channel: Option<String>,
    call: Option<String>,
    answer: bool,
    calls: CallOptions,
    seconds: u64,
}

fn parse() -> Args {
    let mut args = Args {
        address: "127.0.0.1:6667".to_owned(),
        nick: "dumper".to_owned(),
        channel: None,
        call: None,
        answer: false,
        calls: CallOptions::default(),
        seconds: 12,
    };

    let mut positional = 0;
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--test-media" => args.calls.test_media = true,
            "--audio-only" => args.calls.audio_only = true,
            "--call" => args.call = rest.next(),
            "--answer" => args.answer = true,
            "--seconds" => {
                args.seconds = rest.next().and_then(|s| s.parse().ok()).unwrap_or(12);
            }
            _ => {
                match positional {
                    0 => args.address = arg,
                    1 => args.nick = arg,
                    _ => args.channel = Some(arg),
                }
                positional += 1;
            }
        }
    }
    args
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args = parse();

    let (host, port) = args
        .address
        .rsplit_once(':')
        .unwrap_or((args.address.as_str(), "6667"));
    let connect = ConnectConfig::plain(host, port.parse().expect("port should be a number"));

    let mut session = SessionConfig::new(args.nick);
    if let Some(channel) = args.channel {
        session.autojoin = vec![channel.into_bytes()];
    }

    let (events_tx, events_rx) = async_channel::unbounded();
    let (commands_tx, commands_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(connection::run(
        connect,
        session,
        args.calls,
        commands_rx,
        events_tx,
    ));

    // Placed once the connection has settled, the way somebody would.
    if let Some(who) = args.call {
        let commands = commands_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            println!("-> starting a call to {who}");
            let _ = commands.send(UiCommand::Call(CallAction::Start(who)));
        });
    }

    let mut answered = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.seconds);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), events_rx.recv()).await {
            Ok(Ok(event)) => {
                match &event {
                    AppEvent::Line { buffer, line } => {
                        println!(
                            "LINE [{buffer}] {:?} {:?} {}",
                            line.kind, line.who, line.text
                        );
                    }
                    other => println!("{other:?}"),
                }

                // Answering from the event rather than on a timer, exactly as
                // the menu does when it becomes available.
                if let AppEvent::CallState { ringing: true, .. } = event
                    && args.answer
                    && !answered
                {
                    answered = true;
                    println!("-> answering");
                    let _ = commands_tx.send(UiCommand::Call(CallAction::Answer));
                }

                if matches!(event, AppEvent::Disconnected { .. }) {
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => {}
        }
    }

    let _ = commands_tx.send(UiCommand::Quit {
        reason: "done".to_owned(),
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
}
