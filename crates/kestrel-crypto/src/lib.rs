//! Identity, sealed signalling, and short authentication strings.
//!
//! Calls run over a server that relays signalling, so the threat model starts
//! from the assumption that the server is hostile. Everything here exists to
//! make that survivable: payloads it cannot read, identities it cannot
//! substitute without being noticed, and a phrase two people can compare out
//! loud that no attacker in the middle can match.

pub mod identity;
pub mod sas;
pub mod seal;

pub use identity::{Identity, IdentityKey, KnownPeer, KnownPeers, Trust};
pub use sas::{SAS_WORDS, Sas};
pub use seal::{EphemeralKey, SealError, SealingKeys, SharedSecret};
