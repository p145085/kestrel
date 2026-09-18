//! Client-side orchestration.
//!
//! The layer between a client's interface and the machinery underneath it:
//! the call state machine, the media engine and the server connection. Both
//! the terminal client and the window drive this, which is what stops calls
//! from behaving differently depending on which one you are looking at.

pub mod calls;
pub mod crash;
pub mod notice;
pub mod store;

pub use calls::{Calls, PeerControl, PeerKey, Permissions, TaggedMediaEvent};
pub use notice::{Level, Notice};
pub use store::Store;
