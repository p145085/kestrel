//! End-to-end tests over real TCP sockets.
//!
//! The sans-io tests in `kestreld-core` prove the protocol logic. These prove
//! the parts that only exist once there is a socket: line framing across
//! packet boundaries, several clients interleaving, and an abrupt disconnect
//! being noticed.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::oneshot;

use kestreld::config::{AccountConfig, Config};

/// A connected test client.
struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    async fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("should connect");
        let (read, writer) = stream.into_split();
        Self {
            reader: BufReader::new(read),
            writer,
        }
    }

    async fn send(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .expect("should write");
    }

    /// Send a line split across two packets, to exercise reassembly.
    async fn send_split(&mut self, line: &str, at: usize) {
        let full = format!("{line}\r\n");
        let (first, second) = full.split_at(at);
        self.writer.write_all(first.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        self.writer.write_all(second.as_bytes()).await.unwrap();
    }

    /// Read until a line containing `needle` arrives.
    async fn expect(&mut self, needle: &str) -> String {
        let deadline = Duration::from_secs(5);
        let found = tokio::time::timeout(deadline, async {
            loop {
                let mut line = String::new();
                let read = self.reader.read_line(&mut line).await.expect("should read");
                assert_ne!(read, 0, "connection closed while waiting for {needle:?}");
                if line.contains(needle) {
                    return line.trim_end().to_owned();
                }
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("timed out waiting for {needle:?}"))
    }

    async fn register(&mut self, nick: &str) {
        self.send(&format!("NICK {nick}")).await;
        self.send(&format!("USER {nick} 0 * :{nick}")).await;
        self.expect(&format!(" 001 {nick}")).await;
    }
}

/// Start a server on an ephemeral port and return it with a shutdown handle.
async fn start() -> (u16, oneshot::Sender<()>) {
    let config = Config {
        listen: vec!["127.0.0.1:0".parse().unwrap()],
        server_name: "irc.test".to_owned(),
        network_name: "TestNet".to_owned(),
        motd: vec!["A test server.".to_owned()],
        accounts: vec![AccountConfig {
            name: "alice".to_owned(),
            password: "hunter2".to_owned(),
        }],
        ..Config::default()
    };

    let listeners = kestreld::bind(&config).await.expect("should bind");
    let port = kestreld::bound_addresses(&listeners)[0].port();
    let (stop_tx, stop_rx) = oneshot::channel();

    tokio::spawn(async move {
        let _ = kestreld::serve(config, listeners, async {
            let _ = stop_rx.await;
        })
        .await;
    });

    (port, stop_tx)
}

#[tokio::test]
async fn a_client_registers_and_receives_the_welcome_burst() {
    let (port, _stop) = start().await;
    let mut alice = Client::connect(port).await;

    alice.send("NICK alice").await;
    alice.send("USER alice 0 * :Alice").await;

    alice.expect(" 001 alice").await;
    alice.expect("NETWORK=TestNet").await;
    alice.expect("A test server.").await;
    alice.expect(" 376 ").await;
}

#[tokio::test]
async fn two_clients_exchange_channel_messages() {
    let (port, _stop) = start().await;
    let mut alice = Client::connect(port).await;
    let mut bob = Client::connect(port).await;

    alice.register("alice").await;
    bob.register("bob").await;

    alice.send("JOIN #test").await;
    alice.expect("JOIN #test").await;

    bob.send("JOIN #test").await;
    bob.expect("JOIN #test").await;
    alice.expect("bob").await; // alice sees bob's JOIN

    alice.send("PRIVMSG #test :hello bob").await;
    let received = bob.expect("PRIVMSG #test :hello bob").await;
    assert!(
        received.starts_with(":alice!~alice@127.0.0.1 "),
        "got {received}"
    );
}

#[tokio::test]
async fn a_line_split_across_packets_is_reassembled() {
    // The read buffer must not assume one packet is one line; a client that
    // writes a message in two pieces is entirely normal.
    let (port, _stop) = start().await;
    let mut alice = Client::connect(port).await;
    let mut bob = Client::connect(port).await;

    alice.register("alice").await;
    bob.register("bob").await;
    alice.send("JOIN #test").await;
    alice.expect("JOIN #test").await;
    bob.send("JOIN #test").await;
    bob.expect("JOIN #test").await;

    alice
        .send_split("PRIVMSG #test :split across packets", 14)
        .await;
    bob.expect("PRIVMSG #test :split across packets").await;
}

#[tokio::test]
async fn several_commands_in_one_packet_are_all_handled() {
    let (port, _stop) = start().await;
    let mut alice = Client::connect(port).await;

    // Registration in a single write, as most clients actually do it.
    alice
        .writer
        .write_all(b"NICK alice\r\nUSER alice 0 * :Alice\r\nJOIN #test\r\n")
        .await
        .unwrap();

    alice.expect(" 001 alice").await;
    alice.expect("JOIN #test").await;
}

#[tokio::test]
async fn an_abrupt_disconnect_is_announced_to_the_channel() {
    let (port, _stop) = start().await;
    let mut alice = Client::connect(port).await;
    let mut bob = Client::connect(port).await;

    alice.register("alice").await;
    bob.register("bob").await;
    alice.send("JOIN #test").await;
    alice.expect("JOIN #test").await;
    bob.send("JOIN #test").await;
    alice.expect("bob").await;

    // Drop bob's socket without sending QUIT.
    drop(bob);

    let quit = alice.expect("QUIT").await;
    assert!(quit.contains("bob"), "got {quit}");
}

#[tokio::test]
async fn sasl_authentication_works_over_a_socket() {
    use base64::Engine;

    let (port, _stop) = start().await;
    let mut carol = Client::connect(port).await;

    carol.send("CAP LS 302").await;
    carol.expect("sasl=PLAIN,EXTERNAL").await;
    carol.send("CAP REQ :sasl").await;
    carol.expect("ACK :sasl").await;

    carol.send("AUTHENTICATE PLAIN").await;
    carol.expect("AUTHENTICATE +").await;

    let payload = base64::engine::general_purpose::STANDARD.encode(b"\0alice\0hunter2");
    carol.send(&format!("AUTHENTICATE {payload}")).await;
    carol.expect(" 903 ").await;

    carol.send("NICK carol").await;
    carol.send("USER carol 0 * :Carol").await;
    carol.send("CAP END").await;
    carol.expect(" 001 carol").await;
}

#[tokio::test]
async fn an_unparseable_line_does_not_drop_the_connection() {
    // Clients and bouncers emit stray malformed lines; dropping the connection
    // over one is worse than dropping the line.
    let (port, _stop) = start().await;
    let mut alice = Client::connect(port).await;
    alice.register("alice").await;

    alice.writer.write_all(b"\r\n").await.unwrap();
    alice.writer.write_all(b"   \r\n").await.unwrap();
    alice.writer.write_all(b"@tagonly\r\n").await.unwrap();

    alice.send("PING :still here").await;
    alice.expect("PONG").await;
}

#[tokio::test]
async fn an_oversized_line_closes_the_connection() {
    let (port, _stop) = start().await;
    let mut alice = Client::connect(port).await;
    alice.register("alice").await;

    let huge = format!(
        "PRIVMSG #test :{}",
        "x".repeat(kestreld::connection::MAX_LINE)
    );
    alice.send(&huge).await;

    // The server closes rather than buffering without limit. Anything still
    // queued from the welcome burst arrives first, so drain until EOF.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut line = String::new();
            if alice
                .reader
                .read_line(&mut line)
                .await
                .expect("should read")
                == 0
            {
                return;
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "connection should have been closed");
}
