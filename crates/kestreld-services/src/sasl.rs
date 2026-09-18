//! The SASL exchange.
//!
//! This is a sans-io state machine over `AUTHENTICATE` lines. It decodes and
//! reassembles the client's side of the exchange but does not decide whether
//! the credentials are good — that needs the account store, and keeping the
//! two apart means the parsing can be tested adversarially on its own.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Longest payload one `AUTHENTICATE` line may carry.
///
/// A client with more to send splits it into chunks of exactly this size; a
/// chunk shorter than this ends the message.
pub const MAX_CHUNK: usize = 400;

/// Largest credential blob accepted once reassembled.
///
/// Without a cap, a client could stream chunks indefinitely before ever
/// authenticating and make the server buffer all of it.
pub const MAX_CREDENTIALS: usize = 8192;

/// A supported SASL mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// Username and password, in the clear. Safe only under TLS.
    Plain,
    /// Authentication by TLS client certificate fingerprint.
    External,
}

impl Mechanism {
    /// Parse a mechanism name, which is case-insensitive.
    #[must_use]
    pub fn parse(name: &[u8]) -> Option<Self> {
        if name.eq_ignore_ascii_case(b"PLAIN") {
            Some(Self::Plain)
        } else if name.eq_ignore_ascii_case(b"EXTERNAL") {
            Some(Self::External)
        } else {
            None
        }
    }

    /// The name as advertised in `RPL_SASLMECHS` and the `sasl` capability.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Plain => "PLAIN",
            Self::External => "EXTERNAL",
        }
    }

    /// Every mechanism the server offers.
    #[must_use]
    pub fn all() -> [Self; 2] {
        [Self::Plain, Self::External]
    }

    /// The advertised list, as it appears after `sasl=`.
    #[must_use]
    pub fn advertised() -> String {
        Self::all()
            .iter()
            .map(|m| m.name())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Credentials extracted from a completed exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credentials {
    /// A username and password to check against the account store.
    Plain {
        /// The account to act as, if it differs from the one authenticating.
        authzid: Vec<u8>,
        /// The account being authenticated.
        authcid: Vec<u8>,
        /// The password.
        password: Vec<u8>,
    },
    /// Authentication by client certificate; the fingerprint comes from the
    /// TLS layer, not from the client's message.
    External {
        /// The account the client asks to be recognised as, possibly empty.
        authzid: Vec<u8>,
    },
}

/// What the caller should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Reply `AUTHENTICATE <payload>`; `+` when the payload is empty.
    Challenge(Vec<u8>),
    /// More chunks are expected; send nothing.
    NeedMore,
    /// The exchange is complete; verify these credentials.
    Complete(Credentials),
    /// The client sent `AUTHENTICATE *`.
    Aborted,
    /// The exchange failed. The string is for logs, not for the client — a
    /// client learns only that authentication failed.
    Failed(&'static str),
}

/// One client's in-progress SASL exchange.
#[derive(Debug, Clone, Default)]
pub struct SaslSession {
    mechanism: Option<Mechanism>,
    /// Base64 chunks received so far, concatenated.
    buffer: Vec<u8>,
    authenticated: bool,
}

impl SaslSession {
    /// A fresh session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The mechanism in progress, if one has been chosen.
    #[must_use]
    pub fn mechanism(&self) -> Option<Mechanism> {
        self.mechanism
    }

    /// Whether this session has already authenticated successfully.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// Mark the session as having succeeded.
    pub fn mark_authenticated(&mut self) {
        self.authenticated = true;
        self.reset();
    }

    /// Discard any partial exchange.
    pub fn reset(&mut self) {
        self.mechanism = None;
        self.buffer.clear();
    }

    /// Handle one `AUTHENTICATE` parameter.
    pub fn step(&mut self, param: &[u8]) -> Step {
        if param == b"*" {
            self.reset();
            return Step::Aborted;
        }

        // The first AUTHENTICATE names the mechanism.
        let Some(mechanism) = self.mechanism else {
            let Some(mechanism) = Mechanism::parse(param) else {
                self.reset();
                return Step::Failed("unsupported mechanism");
            };
            self.mechanism = Some(mechanism);
            // An empty challenge tells the client to send its response.
            return Step::Challenge(b"+".to_vec());
        };

        if param.len() > MAX_CHUNK {
            self.reset();
            return Step::Failed("chunk longer than 400 bytes");
        }

        // `+` stands for an empty payload, which EXTERNAL normally sends.
        if param != b"+" {
            if self.buffer.len() + param.len() > MAX_CREDENTIALS {
                self.reset();
                return Step::Failed("credentials too long");
            }
            self.buffer.extend_from_slice(param);
        }

        // A chunk of exactly MAX_CHUNK means another follows.
        if param.len() == MAX_CHUNK {
            return Step::NeedMore;
        }

        let encoded = std::mem::take(&mut self.buffer);
        self.mechanism = None;

        let Ok(decoded) = BASE64.decode(&encoded) else {
            return Step::Failed("payload is not valid base64");
        };

        match mechanism {
            Mechanism::Plain => decode_plain(&decoded),
            Mechanism::External => Step::Complete(Credentials::External { authzid: decoded }),
        }
    }
}

/// Decode a PLAIN payload: `authzid NUL authcid NUL password`.
///
/// `authcid` and `authzid` are RFC 4616's own names for these fields; keeping
/// them makes the code checkable against the specification.
#[allow(clippy::similar_names)]
fn decode_plain(decoded: &[u8]) -> Step {
    let mut parts = decoded.splitn(3, |&b| b == 0);
    let (Some(authzid), Some(authcid), Some(password)) = (parts.next(), parts.next(), parts.next())
    else {
        return Step::Failed("PLAIN payload is not three NUL-separated fields");
    };
    if authcid.is_empty() || password.is_empty() {
        return Step::Failed("PLAIN username or password is empty");
    }
    Step::Complete(Credentials::Plain {
        authzid: authzid.to_vec(),
        authcid: authcid.to_vec(),
        password: password.to_vec(),
    })
}

/// Encode a payload for sending, split into chunks the client can reassemble.
///
/// A payload whose encoding is an exact multiple of the chunk size needs a
/// trailing `+`, or the client waits forever for a short chunk that never
/// arrives.
#[must_use]
pub fn encode_chunks(payload: &[u8]) -> Vec<Vec<u8>> {
    let encoded = BASE64.encode(payload);
    if encoded.is_empty() {
        return vec![b"+".to_vec()];
    }
    let mut chunks: Vec<Vec<u8>> = encoded
        .as_bytes()
        .chunks(MAX_CHUNK)
        .map(<[u8]>::to_vec)
        .collect();
    if encoded.len().is_multiple_of(MAX_CHUNK) {
        chunks.push(b"+".to_vec());
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::{
        Credentials, MAX_CHUNK, MAX_CREDENTIALS, Mechanism, SaslSession, Step, encode_chunks,
    };
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;

    #[allow(clippy::similar_names)]
    fn plain_payload(authzid: &str, authcid: &str, password: &str) -> String {
        let raw = format!("{authzid}\0{authcid}\0{password}");
        BASE64.encode(raw)
    }

    #[test]
    fn mechanism_names_are_case_insensitive() {
        assert_eq!(Mechanism::parse(b"plain"), Some(Mechanism::Plain));
        assert_eq!(Mechanism::parse(b"PLAIN"), Some(Mechanism::Plain));
        assert_eq!(Mechanism::parse(b"External"), Some(Mechanism::External));
        assert_eq!(Mechanism::parse(b"SCRAM-SHA-256"), None);
    }

    #[test]
    fn advertised_list_names_every_mechanism() {
        assert_eq!(Mechanism::advertised(), "PLAIN,EXTERNAL");
    }

    #[test]
    fn a_plain_exchange_yields_credentials() {
        let mut session = SaslSession::new();
        assert_eq!(session.step(b"PLAIN"), Step::Challenge(b"+".to_vec()));

        let payload = plain_payload("", "alice", "hunter2");
        assert_eq!(
            session.step(payload.as_bytes()),
            Step::Complete(Credentials::Plain {
                authzid: Vec::new(),
                authcid: b"alice".to_vec(),
                password: b"hunter2".to_vec(),
            })
        );
    }

    #[test]
    fn an_authzid_is_preserved() {
        let mut session = SaslSession::new();
        session.step(b"PLAIN");
        let payload = plain_payload("admin", "alice", "hunter2");
        assert_eq!(
            session.step(payload.as_bytes()),
            Step::Complete(Credentials::Plain {
                authzid: b"admin".to_vec(),
                authcid: b"alice".to_vec(),
                password: b"hunter2".to_vec(),
            })
        );
    }

    #[test]
    fn an_external_exchange_needs_no_payload() {
        let mut session = SaslSession::new();
        assert_eq!(session.step(b"EXTERNAL"), Step::Challenge(b"+".to_vec()));
        assert_eq!(
            session.step(b"+"),
            Step::Complete(Credentials::External {
                authzid: Vec::new()
            })
        );
    }

    #[test]
    fn an_unsupported_mechanism_fails_immediately() {
        let mut session = SaslSession::new();
        assert!(matches!(session.step(b"SCRAM-SHA-256"), Step::Failed(_)));
        assert_eq!(session.mechanism(), None);
    }

    #[test]
    fn a_client_can_abort_at_any_point() {
        let mut session = SaslSession::new();
        session.step(b"PLAIN");
        assert_eq!(session.step(b"*"), Step::Aborted);
        assert_eq!(session.mechanism(), None);
    }

    #[test]
    fn long_payloads_are_reassembled_from_chunks() {
        let password = "p".repeat(700);
        let payload = plain_payload("", "alice", &password);
        assert!(payload.len() > MAX_CHUNK);

        let mut session = SaslSession::new();
        session.step(b"PLAIN");

        let bytes = payload.as_bytes();
        let mut offset = 0;
        while offset + MAX_CHUNK <= bytes.len() {
            assert_eq!(
                session.step(&bytes[offset..offset + MAX_CHUNK]),
                Step::NeedMore
            );
            offset += MAX_CHUNK;
        }
        let step = session.step(&bytes[offset..]);

        assert_eq!(
            step,
            Step::Complete(Credentials::Plain {
                authzid: Vec::new(),
                authcid: b"alice".to_vec(),
                password: password.into_bytes(),
            })
        );
    }

    #[test]
    fn an_oversized_chunk_is_refused() {
        let mut session = SaslSession::new();
        session.step(b"PLAIN");
        let too_long = vec![b'A'; MAX_CHUNK + 1];
        assert!(matches!(session.step(&too_long), Step::Failed(_)));
    }

    #[test]
    fn a_client_cannot_buffer_unbounded_credentials() {
        let mut session = SaslSession::new();
        session.step(b"PLAIN");
        let chunk = vec![b'A'; MAX_CHUNK];

        let mut sent = 0;
        loop {
            match session.step(&chunk) {
                Step::NeedMore => {
                    sent += MAX_CHUNK;
                    assert!(
                        sent <= MAX_CREDENTIALS + MAX_CHUNK,
                        "buffering should have been cut off by now"
                    );
                }
                Step::Failed(_) => break,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn malformed_payloads_fail_rather_than_panic() {
        for bad in ["!!!not base64!!!", "", "AAAA"] {
            let mut session = SaslSession::new();
            session.step(b"PLAIN");
            let step = session.step(bad.as_bytes());
            assert!(matches!(step, Step::Failed(_)), "{bad:?} produced {step:?}");
        }
    }

    #[test]
    fn plain_rejects_empty_username_or_password() {
        for (user, pass) in [("", "hunter2"), ("alice", "")] {
            let mut session = SaslSession::new();
            session.step(b"PLAIN");
            let payload = plain_payload("", user, pass);
            assert!(matches!(session.step(payload.as_bytes()), Step::Failed(_)));
        }
    }

    #[test]
    fn a_password_containing_nul_keeps_its_later_bytes() {
        // splitn(3) means everything after the second NUL is the password,
        // NUL bytes included, rather than being silently truncated.
        let raw = b"\0alice\0pass\0word".to_vec();
        let payload = BASE64.encode(raw);
        let mut session = SaslSession::new();
        session.step(b"PLAIN");
        assert_eq!(
            session.step(payload.as_bytes()),
            Step::Complete(Credentials::Plain {
                authzid: Vec::new(),
                authcid: b"alice".to_vec(),
                password: b"pass\0word".to_vec(),
            })
        );
    }

    #[test]
    fn encoding_an_empty_payload_produces_a_plus() {
        assert_eq!(encode_chunks(b""), vec![b"+".to_vec()]);
    }

    #[test]
    fn an_exact_multiple_gets_a_trailing_plus() {
        // 300 bytes encode to exactly 400 base64 characters.
        let payload = vec![b'x'; 300];
        let chunks = encode_chunks(&payload);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), MAX_CHUNK);
        assert_eq!(chunks[1], b"+");
    }

    #[test]
    fn a_session_can_be_reused_after_an_abort() {
        let mut session = SaslSession::new();
        session.step(b"PLAIN");
        session.step(b"*");

        assert_eq!(session.step(b"PLAIN"), Step::Challenge(b"+".to_vec()));
        let payload = plain_payload("", "alice", "hunter2");
        assert!(matches!(
            session.step(payload.as_bytes()),
            Step::Complete(_)
        ));
    }
}
