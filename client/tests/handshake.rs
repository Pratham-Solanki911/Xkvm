//! Exercises the client's post-TLS handshake (`Hello`/`HelloAck`,
//! `PairRequest`/`PairAccept`) over a plain `tokio::io::duplex` pair against
//! a fake, hand-rolled "server" -- no TLS or real sockets involved, since
//! `perform_handshake` is generic over any `AsyncRead + AsyncWrite` stream.
//! This is what proves the client actually sends a pairing proof computed
//! from the PIN it was given.

use kvm_client::config::Config;
use kvm_client::session::{perform_handshake, ConnectOptions};
use kvm_common::{
    pairing_proof, proof_matches, read_envelope, send_envelope, Envelope, PROTOCOL_VERSION,
};

#[tokio::test]
async fn client_sends_a_proof_matching_the_configured_pin() {
    let (mut client_side, mut server_side) = tokio::io::duplex(64 * 1024);

    let pin = "424242".to_string();
    let server_task = tokio::spawn(async move {
        match read_envelope(&mut server_side).await.unwrap() {
            Envelope::Hello { version, .. } => assert_eq!(version, PROTOCOL_VERSION),
            other => panic!("expected Hello, got {other:?}"),
        }
        send_envelope(
            &mut server_side,
            &Envelope::HelloAck {
                file_transfer_port: 4001,
                server_name: "fake-server".to_string(),
            },
        )
        .await
        .unwrap();

        let (nonce, fingerprint, proof) = match read_envelope(&mut server_side).await.unwrap() {
            Envelope::PairRequest {
                nonce,
                fingerprint,
                proof,
            } => (nonce, fingerprint, proof),
            other => panic!("expected PairRequest, got {other:?}"),
        };

        let expected = pairing_proof("424242", &nonce, &fingerprint);
        assert!(
            proof_matches(&proof, &expected),
            "client's proof should match one computed with the same PIN"
        );

        send_envelope(&mut server_side, &Envelope::PairAccept)
            .await
            .unwrap();
    });

    let mut config = Config::default();
    let opts = ConnectOptions {
        pin: Some(pin),
        trust_new_cert: false,
        expected_fingerprint: None,
    };

    let (file_transfer_port, server_name) = perform_handshake(&mut client_side, &mut config, &opts)
        .await
        .expect("handshake should succeed");

    assert_eq!(file_transfer_port, 4001);
    assert_eq!(server_name, "fake-server");
    assert!(
        !config.client_secret.is_empty(),
        "handshake should generate a client secret"
    );

    server_task.await.unwrap();
}

#[tokio::test]
async fn wrong_pin_rejection_is_reported_as_fatal() {
    let (mut client_side, mut server_side) = tokio::io::duplex(64 * 1024);

    let server_task = tokio::spawn(async move {
        let _ = read_envelope(&mut server_side).await.unwrap(); // Hello
        send_envelope(
            &mut server_side,
            &Envelope::HelloAck {
                file_transfer_port: 4001,
                server_name: "fake-server".to_string(),
            },
        )
        .await
        .unwrap();
        let _ = read_envelope(&mut server_side).await.unwrap(); // PairRequest
        send_envelope(
            &mut server_side,
            &Envelope::PairReject {
                reason: "pairing PIN required or incorrect".to_string(),
            },
        )
        .await
        .unwrap();
    });

    let mut config = Config::default();
    let opts = ConnectOptions {
        pin: Some("000000".to_string()),
        trust_new_cert: false,
        expected_fingerprint: None,
    };

    let err = perform_handshake(&mut client_side, &mut config, &opts)
        .await
        .expect_err("wrong PIN should fail the handshake");
    assert!(err
        .downcast_ref::<kvm_client::FatalConnectError>()
        .is_some());
    assert!(err.to_string().to_lowercase().contains("pin"));

    server_task.await.unwrap();
}

#[tokio::test]
async fn protocol_version_mismatch_is_reported_as_fatal() {
    let (mut client_side, mut server_side) = tokio::io::duplex(64 * 1024);

    let server_task = tokio::spawn(async move {
        let _ = read_envelope(&mut server_side).await.unwrap(); // Hello
        send_envelope(
            &mut server_side,
            &Envelope::Error("protocol version mismatch: server=9.9.9 client=0.2.0".to_string()),
        )
        .await
        .unwrap();
    });

    let mut config = Config::default();
    let opts = ConnectOptions::default();

    let err = perform_handshake(&mut client_side, &mut config, &opts)
        .await
        .expect_err("version mismatch should fail the handshake");
    assert!(err
        .downcast_ref::<kvm_client::FatalConnectError>()
        .is_some());

    server_task.await.unwrap();
}
