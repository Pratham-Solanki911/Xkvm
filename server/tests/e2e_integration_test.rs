use anyhow::Result;
use kvm_common::{
    pairing_proof, read_envelope, send_envelope, Caps, Envelope, Event, PROTOCOL_VERSION,
};
use kvm_server::config::Config;
use kvm_server::file_transfer::FileTransferServer;
use kvm_server::server::{handle_client, ServerState};
use kvm_server::socks5::{Socks5Config, Socks5Server};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_rustls::TlsConnector;
use uuid::Uuid;

fn temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kvm_test_{}_{}", label, Uuid::new_v4()))
}

fn make_state(config: Config, config_path: PathBuf, pin: &str) -> ServerState {
    ServerState {
        config: Arc::new(RwLock::new(config)),
        config_path: Arc::new(config_path),
        forwarding_enabled: Arc::new(AtomicBool::new(false)),
        registry: Arc::new(Mutex::new(HashMap::new())),
        file_port: 4001,
        pin: Arc::new(pin.to_string()),
        rate_limiter: Arc::new(Mutex::new(HashMap::new())),
        clipboard_tx: None,
    }
}

fn loopback_ip() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

/// A standalone permit for tests that drive `handle_client` directly - in
/// production this comes from `run_server`'s pending-connection semaphore.
fn test_permit() -> tokio::sync::OwnedSemaphorePermit {
    Arc::new(tokio::sync::Semaphore::new(1))
        .try_acquire_owned()
        .unwrap()
}

async fn hello_handshake(client: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin)) {
    send_envelope(
        client,
        &Envelope::Hello {
            name: "tester".to_string(),
            version: PROTOCOL_VERSION.to_string(),
            caps: Caps::default(),
        },
    )
    .await
    .unwrap();
    let ack = read_envelope(client).await.unwrap();
    assert!(matches!(ack, Envelope::HelloAck { .. }));
}

// ---------------------------------------------------------------------
// Control channel / pairing, driven directly through `handle_client` over
// an in-memory duplex stream (no real TCP/TLS needed to exercise the
// pairing state machine).
// ---------------------------------------------------------------------

#[tokio::test]
async fn test_correct_pin_pairs_and_persists_to_config() -> Result<()> {
    let dir = temp_dir("pair_persist");
    let config_path = dir.join("server.toml");
    let state = make_state(Config::default(), config_path.clone(), "123456");

    let (mut client, server_stream) = tokio::io::duplex(8192);
    let handle = tokio::spawn(handle_client(
        server_stream,
        state,
        loopback_ip(),
        1,
        test_permit(),
    ));

    hello_handshake(&mut client).await;

    let nonce = [7u8; 16];
    let fingerprint = "fp-correct-pin".to_string();
    let proof = pairing_proof("123456", &nonce, &fingerprint);
    send_envelope(
        &mut client,
        &Envelope::PairRequest {
            nonce,
            fingerprint: fingerprint.clone(),
            proof,
        },
    )
    .await?;
    let resp = read_envelope(&mut client).await?;
    assert!(matches!(resp, Envelope::PairAccept));

    send_envelope(&mut client, &Envelope::Goodbye).await?;
    tokio::time::timeout(Duration::from_secs(2), handle).await???;

    let reloaded = Config::load_or_default(&config_path)?;
    assert!(reloaded.is_paired(&fingerprint));

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

#[tokio::test]
async fn test_wrong_pin_is_rejected() -> Result<()> {
    let dir = temp_dir("wrong_pin");
    let config_path = dir.join("server.toml");
    let state = make_state(Config::default(), config_path, "123456");

    let (mut client, server_stream) = tokio::io::duplex(8192);
    let handle = tokio::spawn(handle_client(
        server_stream,
        state,
        loopback_ip(),
        1,
        test_permit(),
    ));

    hello_handshake(&mut client).await;

    let nonce = [1u8; 16];
    let fingerprint = "fp-wrong-pin".to_string();
    let proof = pairing_proof("000000", &nonce, &fingerprint);
    send_envelope(
        &mut client,
        &Envelope::PairRequest {
            nonce,
            fingerprint,
            proof,
        },
    )
    .await?;
    let resp = read_envelope(&mut client).await?;
    assert!(matches!(resp, Envelope::PairReject { .. }));

    let result = tokio::time::timeout(Duration::from_secs(2), handle).await?;
    assert!(
        result?.is_err(),
        "handler should report the rejected pairing"
    );

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

#[tokio::test]
async fn test_already_paired_ignores_garbage_proof() -> Result<()> {
    let dir = temp_dir("already_paired");
    let config_path = dir.join("server.toml");
    let mut cfg = Config::default();
    cfg.add_paired_device("fp-already".to_string(), "Old Device".to_string());
    let state = make_state(cfg, config_path, "123456");

    let (mut client, server_stream) = tokio::io::duplex(8192);
    let handle = tokio::spawn(handle_client(
        server_stream,
        state,
        loopback_ip(),
        1,
        test_permit(),
    ));

    hello_handshake(&mut client).await;

    send_envelope(
        &mut client,
        &Envelope::PairRequest {
            nonce: [0u8; 16],
            fingerprint: "fp-already".to_string(),
            proof: [0xFFu8; 32], // garbage - must be ignored since already paired
        },
    )
    .await?;
    let resp = read_envelope(&mut client).await?;
    assert!(matches!(resp, Envelope::PairAccept));

    send_envelope(&mut client, &Envelope::Goodbye).await?;
    tokio::time::timeout(Duration::from_secs(2), handle).await???;

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

#[tokio::test]
async fn test_protocol_version_mismatch_sends_error() -> Result<()> {
    let dir = temp_dir("version_mismatch");
    let config_path = dir.join("server.toml");
    let state = make_state(Config::default(), config_path, "123456");

    let (mut client, server_stream) = tokio::io::duplex(8192);
    let handle = tokio::spawn(handle_client(
        server_stream,
        state,
        loopback_ip(),
        1,
        test_permit(),
    ));

    send_envelope(
        &mut client,
        &Envelope::Hello {
            name: "tester".to_string(),
            version: "0.0.1-not-real".to_string(),
            caps: Caps::default(),
        },
    )
    .await?;
    let resp = read_envelope(&mut client).await?;
    assert!(matches!(resp, Envelope::Error(_)));

    let result = tokio::time::timeout(Duration::from_secs(2), handle).await?;
    assert!(result?.is_err());

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

#[tokio::test]
async fn test_client_drop_ends_handler_within_2s() -> Result<()> {
    let dir = temp_dir("client_drop");
    let config_path = dir.join("server.toml");
    let state = make_state(Config::default(), config_path, "123456");

    let (mut client, server_stream) = tokio::io::duplex(8192);
    let handle = tokio::spawn(handle_client(
        server_stream,
        state,
        loopback_ip(),
        1,
        test_permit(),
    ));

    hello_handshake(&mut client).await;
    let nonce = [2u8; 16];
    let fingerprint = "fp-dropper".to_string();
    let proof = pairing_proof("123456", &nonce, &fingerprint);
    send_envelope(
        &mut client,
        &Envelope::PairRequest {
            nonce,
            fingerprint,
            proof,
        },
    )
    .await?;
    let _ = read_envelope(&mut client).await?;

    drop(client); // no Goodbye - the handler must notice EOF and exit anyway

    tokio::time::timeout(Duration::from_secs(2), handle).await???;

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

#[tokio::test]
async fn test_toggle_forward_from_client_flips_shared_flag() -> Result<()> {
    let dir = temp_dir("toggle_forward");
    let config_path = dir.join("server.toml");
    let state = make_state(Config::default(), config_path, "123456");
    let forwarding_enabled = state.forwarding_enabled.clone();
    assert!(!forwarding_enabled.load(Ordering::SeqCst));

    let (mut client, server_stream) = tokio::io::duplex(8192);
    let handle = tokio::spawn(handle_client(
        server_stream,
        state,
        loopback_ip(),
        1,
        test_permit(),
    ));

    hello_handshake(&mut client).await;
    let nonce = [3u8; 16];
    let fingerprint = "fp-toggle".to_string();
    let proof = pairing_proof("123456", &nonce, &fingerprint);
    send_envelope(
        &mut client,
        &Envelope::PairRequest {
            nonce,
            fingerprint,
            proof,
        },
    )
    .await?;
    let _ = read_envelope(&mut client).await?;

    send_envelope(&mut client, &Envelope::ToggleForward(true)).await?;

    // The client should observe the server echoing the toggle back.
    let echoed = tokio::time::timeout(Duration::from_secs(2), read_envelope(&mut client)).await??;
    assert!(matches!(echoed, Envelope::ToggleForward(true)));
    assert!(forwarding_enabled.load(Ordering::SeqCst));

    send_envelope(&mut client, &Envelope::Goodbye).await?;
    tokio::time::timeout(Duration::from_secs(2), handle).await???;

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

// ---------------------------------------------------------------------
// File transfer over TLS
// ---------------------------------------------------------------------

/// A test-only verifier that accepts any server certificate but still
/// performs real TLS1.2/1.3 handshake-signature verification (only the
/// hostname/CA/pinning checks are skipped).
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn build_test_tls() -> (Arc<rustls::ServerConfig>, Arc<rustls::ClientConfig>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_der = CertificateDer::from(cert.serialize_der().unwrap());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.serialize_private_key_der()));

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)
        .unwrap();

    let client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();

    (Arc::new(server_config), Arc::new(client_config))
}

async fn connect_tls(
    port: u16,
    client_tls: Arc<rustls::ClientConfig>,
) -> tokio_rustls::client::TlsStream<TcpStream> {
    let tcp = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .unwrap();
    let connector = TlsConnector::from(client_tls);
    let domain = ServerName::try_from("localhost".to_string()).unwrap();
    connector.connect(domain, tcp).await.unwrap()
}

async fn start_file_server(
    config: Config,
) -> (u16, Arc<rustls::ClientConfig>, Arc<RwLock<Config>>) {
    let (server_tls, client_tls) = build_test_tls();
    let config = Arc::new(RwLock::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let config_clone = config.clone();
    tokio::spawn(async move {
        let _ = FileTransferServer::run("127.0.0.1", port, server_tls, config_clone).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    (port, client_tls, config)
}

async fn pair_on_file_channel(
    stream: &mut tokio_rustls::client::TlsStream<TcpStream>,
    fingerprint: &str,
) -> Envelope {
    send_envelope(
        stream,
        &Envelope::PairRequest {
            nonce: [0u8; 16],
            fingerprint: fingerprint.to_string(),
            proof: pairing_proof("", &[0u8; 16], fingerprint),
        },
    )
    .await
    .unwrap();
    read_envelope(stream).await.unwrap()
}

#[tokio::test]
async fn test_file_transfer_fresh_upload_over_tls() {
    let dir = temp_dir("file_fresh");
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config {
        transfer_dir: dir.clone(),
        ..Config::default()
    };
    cfg.add_paired_device("paired-fp".to_string(), "Tester".to_string());

    let (port, client_tls, _config) = start_file_server(cfg).await;
    let mut stream = connect_tls(port, client_tls).await;

    let pair_resp = pair_on_file_channel(&mut stream, "paired-fp").await;
    assert!(matches!(pair_resp, Envelope::PairAccept));

    let file_id = Uuid::new_v4();
    let payload = b"hello over TLS file transfer";
    let mut hasher = Sha256::new();
    hasher.update(payload);
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&hasher.finalize());

    send_envelope(
        &mut stream,
        &Envelope::FileOffer {
            id: file_id,
            name: "greeting.txt".to_string(),
            size: payload.len() as u64,
            sha256,
        },
    )
    .await
    .unwrap();

    let accept = read_envelope(&mut stream).await.unwrap();
    assert!(matches!(
        accept,
        Envelope::FileAccept {
            start_offset: 0,
            ..
        }
    ));

    stream.write_all(payload).await.unwrap();
    stream.flush().await.unwrap();

    let complete = read_envelope(&mut stream).await.unwrap();
    assert!(matches!(complete, Envelope::FileComplete { id } if id == file_id));

    let final_path = dir.join("greeting.txt");
    assert!(final_path.exists());
    assert_eq!(std::fs::read(&final_path).unwrap(), payload);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_file_transfer_resumes_partial_and_avoids_overwrite() {
    let dir = temp_dir("file_resume");
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config {
        transfer_dir: dir.clone(),
        ..Config::default()
    };
    cfg.add_paired_device("paired-fp".to_string(), "Tester".to_string());

    let (port, client_tls, _config) = start_file_server(cfg).await;

    let full_payload = b"0123456789ABCDEFGHIJ";
    let mut hasher = Sha256::new();
    hasher.update(full_payload);
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&hasher.finalize());
    let file_id = Uuid::new_v4();

    // Simulate a half-finished transfer by pre-creating the `.part` file.
    std::fs::write(dir.join("data.txt.part"), &full_payload[..10]).unwrap();

    let mut stream = connect_tls(port, client_tls.clone()).await;
    let pair_resp = pair_on_file_channel(&mut stream, "paired-fp").await;
    assert!(matches!(pair_resp, Envelope::PairAccept));

    send_envelope(
        &mut stream,
        &Envelope::FileOffer {
            id: file_id,
            name: "data.txt".to_string(),
            size: full_payload.len() as u64,
            sha256,
        },
    )
    .await
    .unwrap();

    let accept = read_envelope(&mut stream).await.unwrap();
    match accept {
        Envelope::FileAccept { start_offset, .. } => assert_eq!(start_offset, 10),
        other => panic!("expected FileAccept, got {:?}", other),
    }

    stream.write_all(&full_payload[10..]).await.unwrap();
    stream.flush().await.unwrap();
    let complete = read_envelope(&mut stream).await.unwrap();
    assert!(matches!(complete, Envelope::FileComplete { id } if id == file_id));
    assert_eq!(std::fs::read(dir.join("data.txt")).unwrap(), full_payload);

    // Sending the same name again must not clobber the existing file.
    let mut stream2 = connect_tls(port, client_tls).await;
    let pair_resp2 = pair_on_file_channel(&mut stream2, "paired-fp").await;
    assert!(matches!(pair_resp2, Envelope::PairAccept));

    let file_id2 = Uuid::new_v4();
    send_envelope(
        &mut stream2,
        &Envelope::FileOffer {
            id: file_id2,
            name: "data.txt".to_string(),
            size: full_payload.len() as u64,
            sha256,
        },
    )
    .await
    .unwrap();
    let _ = read_envelope(&mut stream2).await.unwrap();
    stream2.write_all(full_payload).await.unwrap();
    stream2.flush().await.unwrap();
    let complete2 = read_envelope(&mut stream2).await.unwrap();
    assert!(matches!(complete2, Envelope::FileComplete { .. }));

    assert!(dir.join("data.txt").exists());
    assert!(dir.join("data (1).txt").exists());

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_file_transfer_rejects_unpaired_fingerprint() {
    let dir = temp_dir("file_unpaired");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = Config {
        transfer_dir: dir.clone(),
        ..Config::default()
    };
    // Nobody paired.

    let (port, client_tls, _config) = start_file_server(cfg).await;
    let mut stream = connect_tls(port, client_tls).await;

    let resp = pair_on_file_channel(&mut stream, "never-paired").await;
    assert!(matches!(resp, Envelope::PairReject { .. }));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_file_transfer_rejects_oversize_file() {
    let dir = temp_dir("file_oversize");
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config {
        transfer_dir: dir.clone(),
        max_file_size: 8, // bytes
        ..Config::default()
    };
    cfg.add_paired_device("paired-fp".to_string(), "Tester".to_string());

    let (port, client_tls, _config) = start_file_server(cfg).await;
    let mut stream = connect_tls(port, client_tls).await;
    let pair_resp = pair_on_file_channel(&mut stream, "paired-fp").await;
    assert!(matches!(pair_resp, Envelope::PairAccept));

    send_envelope(
        &mut stream,
        &Envelope::FileOffer {
            id: Uuid::new_v4(),
            name: "too_big.bin".to_string(),
            size: 1024,
            sha256: [0u8; 32],
        },
    )
    .await
    .unwrap();

    let resp = read_envelope(&mut stream).await.unwrap();
    assert!(matches!(resp, Envelope::FileReject { .. }));
    assert!(!dir.join("too_big.bin").exists());
    assert!(!dir.join("too_big.bin.part").exists());

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------
// SOCKS5
// ---------------------------------------------------------------------

#[tokio::test]
async fn test_e2e_socks5_proxy() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy_port = listener.local_addr()?.port();
    drop(listener);

    let socks_cfg = Socks5Config {
        username: None,
        password: None,
        idle_timeout: Duration::from_secs(10),
        allow_anonymous: true,
    };

    tokio::spawn(async move {
        let _ = Socks5Server::run("127.0.0.1", proxy_port, Some(socks_cfg)).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let echo_listener = TcpListener::bind("127.0.0.1:0").await?;
    let echo_port = echo_listener.local_addr()?.port();

    tokio::spawn(async move {
        let (mut stream, _) = echo_listener.accept().await.unwrap();
        let mut buf = [0; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        stream.write_all(&buf[..n]).await.unwrap();
    });

    let mut client = TcpStream::connect(format!("127.0.0.1:{}", proxy_port)).await?;

    client.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut auth_resp = [0u8; 2];
    client.read_exact(&mut auth_resp).await?;
    assert_eq!(auth_resp, [0x05, 0x00]);

    let mut connect_req = vec![0x05, 0x01, 0x00, 0x01];
    connect_req.extend_from_slice(&[127, 0, 0, 1]);
    connect_req.extend_from_slice(&echo_port.to_be_bytes());

    client.write_all(&connect_req).await?;

    let mut connect_resp = [0u8; 10];
    client.read_exact(&mut connect_resp).await?;
    assert_eq!(connect_resp[0], 0x05);
    assert_eq!(connect_resp[1], 0x00);

    client.write_all(b"Hello SOCKS5").await?;

    let mut echo_buf = [0u8; 12];
    client.read_exact(&mut echo_buf).await?;
    assert_eq!(&echo_buf, b"Hello SOCKS5");

    Ok(())
}

#[tokio::test]
async fn test_socks5_anonymous_rejected_by_default() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy_port = listener.local_addr()?.port();
    drop(listener);

    let socks_cfg = Socks5Config {
        username: None,
        password: None,
        idle_timeout: Duration::from_secs(10),
        allow_anonymous: false,
    };

    tokio::spawn(async move {
        let _ = Socks5Server::run("127.0.0.1", proxy_port, Some(socks_cfg)).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(format!("127.0.0.1:{}", proxy_port)).await?;
    client.write_all(&[0x05, 0x01, 0x00]).await?;

    // The server has no credentials configured and anonymous access is
    // disallowed, so it closes the connection instead of proceeding.
    let mut buf = [0u8; 2];
    let result = client.read_exact(&mut buf).await;
    assert!(
        result.is_err(),
        "connection should be closed without a method reply"
    );

    Ok(())
}

// A quick sanity check that a forwarded `Event::ReleaseAll` round-trips
// through the wire protocol used by the control channel (regression guard
// for the new variant).
#[test]
fn test_release_all_event_round_trips() {
    let bytes = kvm_common::serialize_envelope(&Envelope::Input(Event::ReleaseAll)).unwrap();
    let decoded = kvm_common::deserialize_envelope(&bytes[4..]).unwrap();
    assert!(matches!(decoded, Envelope::Input(Event::ReleaseAll)));
}
