use crate::config::Config;
use crate::session::ConnectOptions;
use crate::tls::PinnedCertVerifier;
use anyhow::{Context, Result};
use kvm_common::{pairing_proof, read_envelope, send_envelope, Envelope};
use rustls::pki_types::ServerName;
use rustls::ClientConfig;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::Sender;
use tokio_rustls::TlsConnector;
use tracing::info;
use uuid::Uuid;

/// Send `path` to `host:port` (the server's *file* port -- from
/// `Session::file_transfer_port`, not the control port). Connects fresh over
/// TLS with the same fingerprint pinning as the control channel, pairs
/// (an empty-PIN proof is fine here: the server only checks that this
/// fingerprint is already paired on the file channel), then offers the file.
///
/// `progress`, if given, receives `(bytes_sent, total_bytes)` as the upload
/// proceeds.
pub async fn send_file(
    host: &str,
    port: u16,
    path: &Path,
    config: &Config,
    opts: &ConnectOptions,
    progress: Option<Sender<(u64, u64)>>,
) -> Result<()> {
    if path.is_dir() {
        anyhow::bail!("'{}' is a directory, not a file", path.display());
    }

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("'{}' has no file name", path.display()))?;

    let file_size = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("cannot stat '{}'", path.display()))?
        .len();

    let required_fp = opts
        .expected_fingerprint
        .clone()
        .or_else(|| config.known_fingerprint(&crate::addr::format_host_port(host, port)));

    let observed = Arc::new(Mutex::new(None));
    let verifier = Arc::new(PinnedCertVerifier::new(required_fp, observed));

    let mut tls_config = ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    tls_config.dangerous().set_certificate_verifier(verifier);
    let connector = TlsConnector::from(Arc::new(tls_config));

    let stream = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("failed to connect to {host}:{port}"))?;
    let domain = ServerName::try_from("kvm-server")
        .map_err(|_| anyhow::anyhow!("invalid server name"))?
        .to_owned();
    let mut stream = connector
        .connect(domain, stream)
        .await
        .context("TLS handshake failed on file channel")?;

    let fingerprint = config.client_fingerprint();
    let nonce: [u8; 16] = rand::random();
    let proof = pairing_proof("", &nonce, &fingerprint);
    send_envelope(
        &mut stream,
        &Envelope::PairRequest {
            nonce,
            fingerprint,
            proof,
        },
    )
    .await?;
    match read_envelope(&mut stream).await? {
        Envelope::PairAccept => {}
        Envelope::PairReject { reason } => {
            anyhow::bail!("file channel rejected: {}", reason)
        }
        other => anyhow::bail!("unexpected response on file channel: {:?}", other),
    }

    let mut hasher = Sha256::new();
    {
        let mut file = File::open(path).await?;
        let mut buffer = [0u8; 65536];
        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
    }
    let sha256 = hasher.finalize();

    let offer = Envelope::FileOffer {
        id: Uuid::new_v4(),
        name: file_name.clone(),
        size: file_size,
        sha256: sha256.into(),
    };
    send_envelope(&mut stream, &offer).await?;

    let start_offset = match read_envelope(&mut stream).await? {
        Envelope::FileAccept { start_offset, .. } => start_offset,
        Envelope::FileReject { reason, .. } => {
            anyhow::bail!("server rejected file '{}': {}", file_name, reason)
        }
        other => anyhow::bail!("unexpected response from server: {:?}", other),
    };

    info!(
        "Server accepted file transfer for '{}', starting at offset {}",
        file_name, start_offset
    );

    let mut file = File::open(path).await?;
    if start_offset > 0 {
        file.seek(std::io::SeekFrom::Start(start_offset)).await?;
    }

    let mut sent_bytes = start_offset;
    let mut chunk = vec![0u8; 65536];
    loop {
        let n = file.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        stream.write_all(&chunk[..n]).await?;
        sent_bytes += n as u64;
        if let Some(tx) = &progress {
            let _ = tx.send((sent_bytes, file_size)).await;
        }
    }

    match read_envelope(&mut stream).await? {
        Envelope::FileComplete { .. } => {
            info!(
                "Server verified SHA-256 and completed transfer for '{}'",
                file_name
            );
            Ok(())
        }
        Envelope::FileReject { reason, .. } => {
            anyhow::bail!("server rejected file '{}': {}", file_name, reason)
        }
        Envelope::Error(e) => anyhow::bail!("server error during file transfer: {}", e),
        other => anyhow::bail!("unexpected response from server: {:?}", other),
    }
}
