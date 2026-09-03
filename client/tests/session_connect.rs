//! Regression test for C7: after `Session::connect` succeeds against a real
//! TLS server, the observed certificate fingerprint must be pinned under
//! *both* the control-port address that was dialed and the file-transfer
//! port's address. `file_transfer::send_file` looks up its required
//! fingerprint via `config.known_fingerprint(host:file_transfer_port)`
//! (file_transfer.rs), which is a different key than the control-port
//! address `Session::connect` dials -- without this, the file channel would
//! silently connect with no pin (`required_fp = None` accepts any
//! certificate) whenever the control and file ports differ, which they do
//! by default (4000 vs 4001).

use kvm_client::config::Config;
use kvm_client::session::{ConnectOptions, Session};
use kvm_common::{read_envelope, send_envelope, Envelope};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["kvm-server".to_string()]).unwrap();
    let cert_der = CertificateDer::from(cert.serialize_der().unwrap());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.serialize_private_key_der()));
    (cert_der, key_der)
}

#[tokio::test]
async fn connect_pins_fingerprint_under_both_control_and_file_ports() {
    let (cert_der, key_der) = self_signed();
    let server_tls = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(server_tls));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_port = listener.local_addr().unwrap().port();
    // Deliberately different from the control port, like the real defaults
    // (4000 vs 4001), so the test actually exercises the mismatch.
    let fake_file_port: u16 = control_port.wrapping_add(1).max(1);

    let server_task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = acceptor.accept(tcp).await.unwrap();

        match read_envelope(&mut stream).await.unwrap() {
            Envelope::Hello { .. } => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        send_envelope(
            &mut stream,
            &Envelope::HelloAck {
                file_transfer_port: fake_file_port,
                server_name: "fake-server".to_string(),
            },
        )
        .await
        .unwrap();

        match read_envelope(&mut stream).await.unwrap() {
            Envelope::PairRequest { .. } => {}
            other => panic!("expected PairRequest, got {other:?}"),
        }
        send_envelope(&mut stream, &Envelope::PairAccept)
            .await
            .unwrap();
    });

    let mut config = Config::default();
    let opts = ConnectOptions {
        pin: Some(String::new()),
        // First connection to this address: accept whatever cert is
        // presented (there's nothing pinned yet).
        trust_new_cert: true,
        expected_fingerprint: None,
    };

    let addr = format!("127.0.0.1:{control_port}");
    let session = Session::connect(&addr, &mut config, &opts)
        .await
        .expect("connect should succeed");
    assert_eq!(session.file_transfer_port(), fake_file_port);

    let control_addr = format!("127.0.0.1:{control_port}");
    let file_addr = format!("127.0.0.1:{fake_file_port}");

    let control_fp = config
        .known_fingerprint(&control_addr)
        .expect("control-port fingerprint should be pinned");
    let file_fp = config.known_fingerprint(&file_addr).expect(
        "file-port fingerprint should also be pinned so file_transfer::send_file's own \
         lookup (keyed by host:file_transfer_port) succeeds instead of connecting unpinned (C7)",
    );
    assert_eq!(
        control_fp, file_fp,
        "same server certificate should yield the same fingerprint under both addresses"
    );
    assert!(!control_fp.is_empty());

    server_task.await.unwrap();
}
