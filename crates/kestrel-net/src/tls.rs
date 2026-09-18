//! TLS for the client side.

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio_rustls::TlsConnector;

/// Build a connector, optionally skipping certificate validation.
pub fn connector(danger_accept_invalid_certs: bool) -> Result<TlsConnector> {
    let provider = crate::provider();
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .context("selecting TLS protocol versions")?;

    let config = if danger_accept_invalid_certs {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    };

    Ok(TlsConnector::from(Arc::new(config)))
}

/// Parse a hostname for certificate validation.
pub fn server_name(host: &str) -> Result<ServerName<'static>> {
    ServerName::try_from(host.to_owned())
        .with_context(|| format!("{host} is not a valid server name"))
}

/// Whether a connector built with `danger_accept_invalid_certs` is in use.
///
/// Exposed so a client can say so in its interface. A user who has turned
/// certificate checking off should be able to see that they have.
#[must_use]
pub fn danger_accept_any_certificate(config: &crate::ConnectConfig) -> bool {
    config.tls && config.danger_accept_invalid_certs
}

/// Accepts any server certificate.
///
/// This removes the guarantee that you are talking to the server you named
/// rather than to whoever answered, so it exists only for a self-signed
/// server whose certificate the user has checked by other means.
#[derive(Debug)]
struct AcceptAnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::{connector, danger_accept_any_certificate, server_name};
    use crate::ConnectConfig;

    #[test]
    fn a_validating_connector_builds() {
        assert!(connector(false).is_ok());
    }

    #[test]
    fn a_permissive_connector_builds() {
        assert!(connector(true).is_ok());
    }

    #[test]
    fn hostnames_are_validated_before_use() {
        assert!(server_name("irc.example.org").is_ok());
        assert!(server_name("192.168.1.100").is_ok());
        assert!(server_name("not a hostname").is_err());
    }

    #[test]
    fn the_permissive_flag_only_counts_when_tls_is_on() {
        let mut config = ConnectConfig::plain("host", 6667);
        config.danger_accept_invalid_certs = true;
        assert!(
            !danger_accept_any_certificate(&config),
            "a plaintext connection has no certificate to accept"
        );

        let mut config = ConnectConfig::tls("host", 6697);
        config.danger_accept_invalid_certs = true;
        assert!(danger_accept_any_certificate(&config));
    }
}
