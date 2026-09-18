//! Print the events the interface would receive, without opening a window.
//!
//! What the client shows is decided entirely by these, so being able to read
//! them is the difference between diagnosing a display bug and guessing at it.
//!
//!     cargo run -p kestrel-ui --example dump_events -- 127.0.0.1:6667 nick #chan

use kestrel_net::ConnectConfig;
use kestrel_session::SessionConfig;
use kestrel_ui::connection;
use kestrel_ui::event::AppEvent;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "127.0.0.1:6667".to_owned());
    let nick = args.next().unwrap_or_else(|| "dumper".to_owned());
    let channel = args.next();

    let (host, port) = address
        .rsplit_once(':')
        .unwrap_or((address.as_str(), "6667"));
    let connect = ConnectConfig::plain(host, port.parse().expect("port should be a number"));

    let mut session = SessionConfig::new(nick);
    if let Some(channel) = channel {
        session.autojoin = vec![channel.into_bytes()];
    }

    let (events_tx, events_rx) = async_channel::unbounded();
    let (commands_tx, commands_rx) = tokio::sync::mpsc::unbounded_channel();
    std::mem::forget(commands_tx);

    tokio::spawn(connection::run(connect, session, commands_rx, events_tx));

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(12);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_secs(2), events_rx.recv()).await {
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
                if matches!(event, AppEvent::Disconnected { .. }) {
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => {}
        }
    }
}
