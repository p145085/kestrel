//! Compact session descriptions and sealed signalling frames.
//!
//! What travels over IRC for a call: a few hundred bytes per peer instead of
//! the kilobytes a raw WebRTC offer would be, sealed so the server relaying it
//! cannot read it.

pub mod csd;
pub mod frame;
pub mod sdp;

pub use csd::{Candidate, CandidateKind, Profile, SessionDescription, Setup};
pub use frame::{
    Frame, FrameError, MediaWanted, binding_context, decode_plain, encode_plain, open, seal,
};
