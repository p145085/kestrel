//! Sans-io IRC client session.
//!
//! Mirrors `kestreld-core` on the client side: messages come in, actions and
//! events come out, and nothing here opens a socket or reads a clock. The GTK
//! client and the headless one both drive this crate, so what a user sees in
//! one is what a test asserts in the other.

pub mod config;
pub mod event;
pub mod isupport;
pub mod session;
pub mod state;

pub use config::{DEFAULT_CAPS, Sasl, SessionConfig};
pub use event::{Ended, Event, MessageKind, Sender, Target};
pub use isupport::{ISupport, Prefix};
pub use session::{Action, Outcome, Session};
pub use state::{Channel, Member};
