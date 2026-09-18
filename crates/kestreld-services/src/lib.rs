//! Accounts, password storage and SASL authentication.
//!
//! Identity on an IRC network is weak by default: a nickname is whoever holds
//! it this second. Kestrel binds call identity to a *services account* instead,
//! which is why this crate is a prerequisite for calls rather than an optional
//! convenience.
//!
//! Everything here is sans-io. The SASL exchange is a state machine over
//! `AUTHENTICATE` lines, and the account store keeps no database handle, so
//! both can be tested adversarially without a socket or a disk.

pub mod account;
pub mod password;
pub mod sasl;

pub use account::{Account, AccountStore, AuthOutcome, RegisterError};
pub use sasl::{Credentials, Mechanism, SaslSession, Step};
