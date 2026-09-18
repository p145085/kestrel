//! TLS support, including client-certificate fingerprints for SASL EXTERNAL.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsAcceptor;

/// Build a TLS acceptor from a certificate chain and private key on disk.
pub fn acceptor(certificate_path: &Path, key_path: &Path) -> Result<TlsAcceptor> {
    let certificates = load_certificates(certificate_path)?;
    let key = load_private_key(key_path)?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let config = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .context("selecting TLS protocol versions")?
        // Client certificates are accepted but never required: CertFP is how a
        // user proves an identity they already registered, not a condition of
        // connecting at all.
        .with_client_cert_verifier(Arc::new(AcceptAnyClientCertificate { provider }))
        .with_single_cert(certificates, key)
        .context("loading the TLS certificate and key")?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let certificates: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(file))
        .collect::<Result<_, _>>()
        .with_context(|| format!("reading certificates from {}", path.display()))?;
    if certificates.is_empty() {
        bail!("{} contains no certificates", path.display());
    }
    Ok(certificates)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    rustls_pemfile::private_key(&mut BufReader::new(file))
        .with_context(|| format!("reading a private key from {}", path.display()))?
        .with_context(|| format!("{} contains no private key", path.display()))
}

/// The SHA-256 fingerprint of a certificate, lowercase hex.
///
/// This is the same value OpenSSL prints for `-fingerprint -sha256`, minus the
/// colons, and what other ircds call a CertFP.
#[must_use]
pub fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(certificate.as_ref());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// A client-certificate verifier that accepts any certificate.
///
/// This looks alarming and is not. A CertFP identifies a user by the exact
/// certificate they registered, so there is no certificate authority in the
/// picture and nothing for a chain to be validated against — the fingerprint
/// *is* the credential. What must still hold is that the client actually
/// possesses the private key, and that is exactly what the handshake signature
/// checks below prove. Accepting the certificate here only means "carry on and
/// let the server decide what this fingerprint is worth", which for an
/// unregistered fingerprint is nothing at all.
#[derive(Debug)]
struct AcceptAnyClientCertificate {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ClientCertVerifier for AcceptAnyClientCertificate {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }

    /// Client certificates are optional.
    fn client_auth_mandatory(&self) -> bool {
        false
    }

    /// Ask for one, so that clients configured with a certificate present it.
    fn offer_client_auth(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::fingerprint;
    use rustls::pki_types::CertificateDer;

    #[test]
    fn fingerprints_are_lowercase_hex_sha256() {
        let certificate = CertificateDer::from(b"not a real certificate".to_vec());
        let printed = fingerprint(&certificate);

        assert_eq!(printed.len(), 64, "SHA-256 is 32 bytes of hex");
        assert!(
            printed
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "got {printed}"
        );
    }

    #[test]
    fn fingerprints_match_a_known_sha256() {
        // echo -n "abc" | sha256sum
        let certificate = CertificateDer::from(b"abc".to_vec());
        assert_eq!(
            fingerprint(&certificate),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn different_certificates_have_different_fingerprints() {
        let a = CertificateDer::from(b"certificate a".to_vec());
        let b = CertificateDer::from(b"certificate b".to_vec());
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }
}
