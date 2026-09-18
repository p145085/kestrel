//! Password hashing.
//!
//! Argon2id, through the PHC string format, so the parameters travel with each
//! hash and can be raised later without invalidating existing accounts.
//!
//! A stolen account database is the realistic threat here — an IRC network is
//! not a high-value target, but its users reuse passwords elsewhere, which is
//! what actually gets them hurt. That rules out fast hashes entirely.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand_core::OsRng;

/// Why a password could not be hashed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HashError {
    /// The hashing itself failed, which should not happen for valid input.
    #[error("could not hash password: {0}")]
    Backend(String),
    /// The password was empty.
    #[error("password is empty")]
    Empty,
    /// The password was longer than the accepted maximum.
    #[error("password is longer than {max} bytes")]
    TooLong {
        /// The accepted maximum.
        max: usize,
    },
}

/// Longest password accepted.
///
/// Argon2 itself has no meaningful limit, but an unbounded one lets anyone
/// spend a lot of the server's CPU with a single registration.
pub const MAX_PASSWORD_LEN: usize = 512;

/// Hash a password, returning a PHC string suitable for storage.
pub fn hash(password: &[u8]) -> Result<String, HashError> {
    if password.is_empty() {
        return Err(HashError::Empty);
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err(HashError::TooLong {
            max: MAX_PASSWORD_LEN,
        });
    }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password, &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| HashError::Backend(e.to_string()))
}

/// Whether `password` matches a stored PHC string.
///
/// Any malformed stored hash verifies as false rather than erroring: a
/// corrupt row should lock one account out, not take the server down.
#[must_use]
pub fn verify(password: &[u8], stored: &str) -> bool {
    if password.is_empty() || password.len() > MAX_PASSWORD_LEN {
        return false;
    }
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default().verify_password(password, &parsed).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{HashError, MAX_PASSWORD_LEN, hash, verify};

    #[test]
    fn a_hashed_password_verifies() {
        let stored = hash(b"correct horse battery staple").unwrap();
        assert!(verify(b"correct horse battery staple", &stored));
    }

    #[test]
    fn a_wrong_password_does_not_verify() {
        let stored = hash(b"hunter2").unwrap();
        assert!(!verify(b"hunter3", &stored));
        assert!(!verify(b"", &stored));
        assert!(!verify(b"hunter2 ", &stored));
    }

    #[test]
    fn hashes_are_salted_so_equal_passwords_differ() {
        let a = hash(b"same").unwrap();
        let b = hash(b"same").unwrap();
        assert_ne!(
            a, b,
            "identical passwords must not produce identical hashes"
        );
        assert!(verify(b"same", &a));
        assert!(verify(b"same", &b));
    }

    #[test]
    fn stored_hashes_use_argon2id() {
        let stored = hash(b"whatever").unwrap();
        assert!(stored.starts_with("$argon2id$"), "got {stored}");
    }

    #[test]
    fn a_corrupt_stored_hash_fails_closed() {
        for bad in ["", "not a hash", "$argon2id$garbage", "$unknown$v=1$x"] {
            assert!(!verify(b"hunter2", bad), "{bad} should not verify");
        }
    }

    #[test]
    fn empty_and_oversized_passwords_are_refused() {
        assert_eq!(hash(b""), Err(HashError::Empty));
        let long = vec![b'a'; MAX_PASSWORD_LEN + 1];
        assert_eq!(
            hash(&long),
            Err(HashError::TooLong {
                max: MAX_PASSWORD_LEN
            })
        );
    }

    #[test]
    fn a_password_at_the_limit_is_accepted() {
        let at_limit = vec![b'a'; MAX_PASSWORD_LEN];
        let stored = hash(&at_limit).unwrap();
        assert!(verify(&at_limit, &stored));
    }
}
