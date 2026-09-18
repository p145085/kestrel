//! Sans-io IRC server state machine.
//!
//! [`Server`] owns all protocol state — clients, nicknames, channels — and
//! decides what happens in response to input. It never opens a socket and
//! never reads the clock: messages and the current time come in as arguments,
//! and [`Action`]s come out for the caller to perform.
//!
//! That is what makes the server's behaviour testable in-process. A test can
//! connect ten clients, run a netsplit, and assert on the exact bytes each one
//! receives, deterministically and in microseconds.
//!
//! ```
//! use kestrel_proto::Message;
//! use kestreld_core::{Server, ServerConfig};
//!
//! let mut server = Server::new(ServerConfig::default(), 0);
//! let alice = server.connect(b"localhost".to_vec());
//!
//! let actions = server.handle_to_vec(alice, &Message::parse(b"NICK alice")?, 0);
//! assert!(actions.is_empty(), "registration is not complete yet");
//!
//! let actions = server.handle_to_vec(alice, &Message::parse(b"USER a 0 * :Alice")?, 0);
//! assert!(!actions.is_empty(), "USER completes registration");
//! assert!(server.client(alice).unwrap().is_registered());
//! # Ok::<(), kestrel_proto::ParseError>(())
//! ```

pub mod channel;
pub mod client;
pub mod commands;
pub mod config;
pub mod datetime;
mod mutate;
pub mod register;
pub mod server;

pub use channel::{Channel, ChannelModes, MemberStatus, Topic};
pub use client::{Client, ClientId, RegistrationState};
pub use config::ServerConfig;
pub use server::{Action, Server};
