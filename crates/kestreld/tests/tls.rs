//! TLS, and authenticating by client certificate.
//!
//! Certificates are generated in-process so that running the suite needs no
//! OpenSSL and no checked-in key material.

use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio_rustls::TlsConnector;

use kestreld::config::{AccountConfig, Config};

/// Accepts any server certificate. Test-only: the server here is self-signed
/// and generated seconds earlier, so there is nothing to chain it to.
#[derive(Debug)]
struct AcceptAnyServer(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAnyServer {
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

/// A generated certificate and its key, in PEM.
struct Generated {
    certificate_pem: String,
    key_pem: String,
    /// SHA-256 fingerprint of the DER form, as the server computes it.
    fingerprint: String,
}

fn generate(name: &str) -> Generated {
    let key = rcgen::generate_simple_self_signed(vec![name.to_owned()]).expect("should generate");
    let der = CertificateDer::from(key.cert.der().to_vec());
    Generated {
        certificate_pem: key.cert.pem(),
        key_pem: key.key_pair.serialize_pem(),
        fingerprint: kestreld::tls::fingerprint(&der),
    }
}

/// Start a TLS server. Returns the port, the client-certificate fingerprint it
/// has registered to `alice`, and a shutdown handle.
async fn start_tls(client_fingerprint: Option<&str>) -> (u16, oneshot::Sender<()>) {
    let server_cert = generate("localhost");
    let dir = tempfile::tempdir().expect("should create a temp dir");
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, &server_cert.certificate_pem).unwrap();
    std::fs::write(&key_path, &server_cert.key_pem).unwrap();

    let config = Config {
        listen: Vec::new(),
        tls_listen: vec!["127.0.0.1:0".parse().unwrap()],
        tls_certificate: Some(cert_path.clone()),
        tls_key: Some(key_path.clone()),
        server_name: "irc.test".to_owned(),
        accounts: vec![AccountConfig {
            name: "alice".to_owned(),
            password: "hunter2".to_owned(),
        }],
        ..Config::default()
    };

    let acceptor = kestreld::tls::acceptor(&cert_path, &key_path).expect("should build acceptor");
    let listeners = kestreld::bind_tls(&config).await.expect("should bind");
    let port = kestreld::bound_addresses(&listeners)[0].port();
    let (stop_tx, stop_rx) = oneshot::channel();

    // The state machine is built inside `serve_all`, so a certificate that
    // needs registering is added through the config-driven account plus a
    // direct call once the server exists. Since `serve_all` owns the server,
    // registration of the fingerprint is expressed as an extra account entry.
    let fingerprint = client_fingerprint.map(str::to_owned);

    tokio::spawn(async move {
        // Keep the temp dir alive for the lifetime of the server.
        let _dir = dir;
        let _ = kestreld::serve_all_with(
            config,
            Vec::new(),
            listeners,
            Some(acceptor),
            move |server| {
                if let Some(fingerprint) = &fingerprint {
                    server.accounts_mut().add_certificate(b"alice", fingerprint);
                }
            },
            async {
                let _ = stop_rx.await;
            },
        )
        .await;
    });

    (port, stop_tx)
}

/// Connect over TLS, optionally presenting a client certificate.
async fn connect_tls(
    port: u16,
    client: Option<&Generated>,
) -> BufReader<tokio_rustls::client::TlsStream<TcpStream>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServer(provider)));

    let config = match client {
        Some(generated) => {
            let certs: Vec<CertificateDer<'static>> =
                rustls_pemfile::certs(&mut generated.certificate_pem.as_bytes())
                    .collect::<Result<_, _>>()
                    .unwrap();
            let key: PrivateKeyDer<'static> =
                rustls_pemfile::private_key(&mut generated.key_pem.as_bytes())
                    .unwrap()
                    .unwrap();
            builder.with_client_auth_cert(certs, key).unwrap()
        }
        None => builder.with_no_client_auth(),
    };

    let connector = TlsConnector::from(Arc::new(config));
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let name = ServerName::try_from("localhost").unwrap();
    BufReader::new(
        connector
            .connect(name, stream)
            .await
            .expect("TLS handshake"),
    )
}

async fn send(stream: &mut BufReader<tokio_rustls::client::TlsStream<TcpStream>>, line: &str) {
    stream
        .get_mut()
        .write_all(format!("{line}\r\n").as_bytes())
        .await
        .unwrap();
}

async fn expect(
    stream: &mut BufReader<tokio_rustls::client::TlsStream<TcpStream>>,
    needle: &str,
) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut line = String::new();
            let read = stream.read_line(&mut line).await.expect("should read");
            assert_ne!(read, 0, "closed while waiting for {needle:?}");
            if line.contains(needle) {
                return line.trim_end().to_owned();
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}"))
}

#[tokio::test]
async fn a_client_registers_over_tls() {
    let (port, _stop) = start_tls(None).await;
    let mut alice = connect_tls(port, None).await;

    send(&mut alice, "NICK alice").await;
    send(&mut alice, "USER alice 0 * :Alice").await;
    expect(&mut alice, " 001 alice").await;
}

#[tokio::test]
async fn sasl_external_authenticates_a_registered_client_certificate() {
    let client = generate("alice");
    let (port, _stop) = start_tls(Some(&client.fingerprint)).await;
    let mut alice = connect_tls(port, Some(&client)).await;

    send(&mut alice, "CAP LS 302").await;
    expect(&mut alice, "sasl=PLAIN,EXTERNAL").await;
    send(&mut alice, "CAP REQ :sasl").await;
    expect(&mut alice, "ACK :sasl").await;

    send(&mut alice, "AUTHENTICATE EXTERNAL").await;
    expect(&mut alice, "AUTHENTICATE +").await;
    send(&mut alice, "AUTHENTICATE +").await;
    // RPL_LOGGEDIN (900) precedes RPL_SASLSUCCESS (903), so expect them in
    // that order — the reader consumes lines as it scans.
    expect(&mut alice, "logged in as alice").await;
    expect(&mut alice, " 903 ").await;
}

#[tokio::test]
async fn sasl_external_fails_for_an_unregistered_certificate() {
    // A certificate the server has never seen must be worth nothing, which is
    // what makes accepting any client certificate at the TLS layer safe.
    let registered = generate("alice");
    let stranger = generate("mallory");
    let (port, _stop) = start_tls(Some(&registered.fingerprint)).await;

    let mut mallory = connect_tls(port, Some(&stranger)).await;
    send(&mut mallory, "CAP REQ :sasl").await;
    expect(&mut mallory, "ACK :sasl").await;
    send(&mut mallory, "AUTHENTICATE EXTERNAL").await;
    expect(&mut mallory, "AUTHENTICATE +").await;
    send(&mut mallory, "AUTHENTICATE +").await;
    expect(&mut mallory, " 904 ").await;
}

#[tokio::test]
async fn sasl_external_fails_without_a_client_certificate() {
    let registered = generate("alice");
    let (port, _stop) = start_tls(Some(&registered.fingerprint)).await;

    let mut alice = connect_tls(port, None).await;
    send(&mut alice, "CAP REQ :sasl").await;
    expect(&mut alice, "ACK :sasl").await;
    send(&mut alice, "AUTHENTICATE EXTERNAL").await;
    expect(&mut alice, "AUTHENTICATE +").await;
    send(&mut alice, "AUTHENTICATE +").await;
    expect(&mut alice, " 904 ").await;
}

#[tokio::test]
async fn two_clients_chat_over_tls() {
    let (port, _stop) = start_tls(None).await;
    let mut alice = connect_tls(port, None).await;
    let mut bob = connect_tls(port, None).await;

    send(&mut alice, "NICK alice").await;
    send(&mut alice, "USER alice 0 * :Alice").await;
    expect(&mut alice, " 001 ").await;
    send(&mut bob, "NICK bob").await;
    send(&mut bob, "USER bob 0 * :Bob").await;
    expect(&mut bob, " 001 ").await;

    send(&mut alice, "JOIN #test").await;
    expect(&mut alice, "JOIN #test").await;
    send(&mut bob, "JOIN #test").await;
    expect(&mut bob, "JOIN #test").await;

    send(&mut alice, "PRIVMSG #test :hello over tls").await;
    expect(&mut bob, "PRIVMSG #test :hello over tls").await;
}
