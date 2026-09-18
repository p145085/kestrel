//! The parts of Kestrel's graphical client that are not graphical.
//!
//! Command parsing, event routing and the connection actor live here rather
//! than in the binary so they can be tested without a display server, which is
//! most of what makes this client's behaviour verifiable in CI at all.

pub mod command;
pub mod connection;
pub mod devices;
pub mod event;
pub mod translate;
