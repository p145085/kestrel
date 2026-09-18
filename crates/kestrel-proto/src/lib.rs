//! Sans-io IRC protocol codec.
//!
//! This crate turns bytes into [`Message`]s and back, and does nothing else:
//! no sockets, no async runtime, no clock. Both the Kestrel client and the
//! Kestrel server depend on it, which is what stops the two drifting apart on
//! the wire.
//!
//! ```
//! use kestrel_proto::Message;
//!
//! let line = b":nick!user@host PRIVMSG #chan :hello world";
//! let msg = Message::parse(line)?;
//!
//! assert!(msg.is_command(b"privmsg"));
//! assert_eq!(msg.param(0), Some(&b"#chan"[..]));
//! assert_eq!(msg.param(1), Some(&b"hello world"[..]));
//! assert_eq!(msg.source().unwrap().nick, b"nick");
//! # Ok::<(), kestrel_proto::ParseError>(())
//! ```

pub mod error;
pub mod limits;
pub mod message;
pub mod source;
pub mod tags;

pub use error::{ParseError, SerializeError};
pub use message::{Message, ParamVec};
pub use source::Source;
pub use tags::{Tag, TagVec};
