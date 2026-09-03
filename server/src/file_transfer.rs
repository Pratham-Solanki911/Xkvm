use crate::config::Config;
use anyhow::Result;
use kvm_common::{read_envelope, send_envelope, Envelope};
use rustls::ServerConfig;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};

/// Same deadline used by the control channel for the TLS handshake and the
/// pre-pairing `PairRequest` read: an attacker who opens a connection and
/// never sends anything must not be able to tie up a task/socket forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Caps how many file-transfer connections may be mid-handshake/mid-pairing
/// at once, mirroring the control channel's pending-connection cap.
const MAX_PENDING_CONNECTIONS: usize = 256;

pub struct FileTransferServer;

impl FileTransferServer {
    pub async fn run(
        bind_address: &str,
        port: u16,
        tls_config: Arc<ServerConfig>,
        config: Arc<RwLock<Config>>,
    ) -> Result<()> {
        let addr = format!("{}:{}", bind_address, port);
        let listener = TcpListener::bind(&addr).await?;
        info!("File transfer server listening on {}", addr);

        let acceptor = TlsAcceptor::from(tls_config);
        let pending_connections = Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_CONNECTIONS));

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let permit = match pending_connections.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            warn!(
                                "Too many pending file-transfer connections; dropping {}",
                                addr
                            );
                            continue;
                        }
                    };
                    info!("File transfer connection from {}", addr);
                    let config = config.clone();
                    let acceptor = acceptor.clone();

                    tokio::spawn(async move {
                        let _permit = permit;
                        match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await
                        {
                            Ok(Ok(tls_stream)) => {
                                if let Err(e) = Self::handle_transfer(tls_stream, config).await {
                                    error!("File transfer error from {}: {}", addr, e);
                                }
                            }
                            Ok(Err(e)) => {
                                error!("File transfer TLS handshake failed for {}: {}", addr, e);
                            }
                            Err(_) => {
                                warn!("File transfer TLS handshake timed out for {}", addr);
                            }
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept file transfer connection: {}", e);
                }
            }
        }
    }

    async fn handle_transfer<S>(mut stream: S, config: Arc<RwLock<Config>>) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        // The file channel reuses the control channel's pairing check: any
        // proof is accepted (it's TLS-protected already), but the
        // fingerprint must already be a paired device.
        let pair_req = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_envelope(&mut stream))
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for PairRequest on file channel"))??;
        match pair_req {
            Envelope::PairRequest { fingerprint, .. } => {
                let is_paired = { config.read().await.is_paired(&fingerprint) };
                if is_paired {
                    send_envelope(&mut stream, &Envelope::PairAccept).await?;
                } else {
                    send_envelope(
                        &mut stream,
                        &Envelope::PairReject {
                            reason: "device not paired; pair over the control channel first"
                                .to_string(),
                        },
                    )
                    .await?;
                    anyhow::bail!("unpaired device attempted file transfer: {}", fingerprint);
                }
            }
            _ => {
                send_envelope(
                    &mut stream,
                    &Envelope::Error("expected PairRequest".to_string()),
                )
                .await?;
                anyhow::bail!("expected PairRequest on file channel");
            }
        }

        let offer = read_envelope(&mut stream).await?;
        let (id, name, size, sha256) = match offer {
            Envelope::FileOffer {
                id,
                name,
                size,
                sha256,
            } => (id, name, size, sha256),
            _ => anyhow::bail!("Expected FileOffer"),
        };

        let safe_name = sanitize_file_name(&name);

        let (downloads_dir, max_file_size) = {
            let cfg = config.read().await;
            (cfg.transfer_dir.clone(), cfg.max_file_size)
        };

        if max_file_size != 0 && size > max_file_size {
            warn!(
                "Rejecting '{}': {} bytes exceeds the configured limit of {} bytes",
                safe_name, size, max_file_size
            );
            send_envelope(
                &mut stream,
                &Envelope::FileReject {
                    id,
                    reason: "file exceeds maximum allowed size".to_string(),
                },
            )
            .await?;
            return Ok(());
        }

        if !downloads_dir.exists() {
            tokio::fs::create_dir_all(&downloads_dir).await?;
        }

        let part_path = downloads_dir.join(format!("{}.part", safe_name));

        let (mut file, start_offset) = if part_path.exists() {
            let existing_size = tokio::fs::metadata(&part_path).await?.len();
            if existing_size < size {
                info!(
                    "Resuming download for '{}' at offset {}",
                    safe_name, existing_size
                );
                (
                    OpenOptions::new().append(true).open(&part_path).await?,
                    existing_size,
                )
            } else {
                (File::create(&part_path).await?, 0)
            }
        } else {
            (File::create(&part_path).await?, 0)
        };

        send_envelope(&mut stream, &Envelope::FileAccept { id, start_offset }).await?;

        info!("Receiving file '{}' ({} bytes)", safe_name, size);

        let remaining = size - start_offset;
        let mut limited = (&mut stream).take(remaining);

        let mut received_bytes = start_offset;
        let mut last_log_bytes = start_offset;
        let log_threshold = std::cmp::min(size / 10, 10 * 1024 * 1024).max(1);

        loop {
            let mut chunk = vec![0u8; 65536];
            let bytes_read = limited.read(&mut chunk).await?;
            if bytes_read == 0 {
                break;
            }

            file.write_all(&chunk[..bytes_read]).await?;
            received_bytes += bytes_read as u64;

            if received_bytes - last_log_bytes >= log_threshold {
                let percent = (received_bytes as f64 / size as f64) * 100.0;
                info!(
                    "Receiving '{}': {:.1}% ({}/{})",
                    safe_name, percent, received_bytes, size
                );
                last_log_bytes = received_bytes;
            }

            if received_bytes >= size {
                break;
            }
        }

        file.flush().await?;
        drop(file);

        if received_bytes != size {
            warn!(
                "File '{}' transfer incomplete ({}/{} bytes received)",
                safe_name, received_bytes, size
            );
            let _ = tokio::fs::remove_file(&part_path).await;
            let _ = send_envelope(
                &mut stream,
                &Envelope::FileReject {
                    id,
                    reason: "transfer incomplete".to_string(),
                },
            )
            .await;
            return Ok(());
        }

        // Verify SHA256 checksum before promoting the .part file.
        let mut verify_file = tokio::fs::File::open(&part_path).await?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let bytes_read = verify_file.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }
        drop(verify_file);
        let calculated_sha256 = hasher.finalize();

        if calculated_sha256.as_slice() == sha256.as_slice() {
            let final_path = unique_dest_path(&downloads_dir, &safe_name);
            tokio::fs::rename(&part_path, &final_path).await?;
            info!(
                "File '{}' received and verified -> {}",
                safe_name,
                final_path.display()
            );
            let _ = send_envelope(&mut stream, &Envelope::FileComplete { id }).await;
        } else {
            warn!("SHA256 checksum mismatch for '{}'", safe_name);
            let _ = tokio::fs::remove_file(&part_path).await;
            let _ = send_envelope(
                &mut stream,
                &Envelope::FileReject {
                    id,
                    reason: "Checksum mismatch".to_string(),
                },
            )
            .await;
        }

        Ok(())
    }
}

/// Reduces `name` to a bare, safe file name: no directory components, and no
/// empty/`.`/`..`/control-character names.
fn sanitize_file_name(name: &str) -> String {
    let candidate = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim();

    let is_bad = candidate.is_empty()
        || candidate == "."
        || candidate == ".."
        || candidate.chars().any(|c| c.is_control());

    if is_bad {
        "received_file".to_string()
    } else {
        candidate.to_string()
    }
}

/// Finds a destination path in `dir` for `name` that does not collide with an
/// existing *final* file, appending " (1)", " (2)", ... before the
/// extension as needed. Never overwrites or deletes an existing file.
fn unique_dest_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }

    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = Path::new(name).extension().and_then(|s| s.to_str());

    for i in 1u64.. {
        let candidate_name = match ext {
            Some(e) => format!("{} ({}).{}", stem, i, e),
            None => format!("{} ({})", stem, i),
        };
        let candidate = dir.join(&candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("dir contains an unbounded number of colliding names")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_file_name_rejects_traversal_and_empty() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name(""), "received_file");
        assert_eq!(sanitize_file_name("."), "received_file");
        assert_eq!(sanitize_file_name(".."), "received_file");
        assert_eq!(sanitize_file_name("a\0b"), "received_file");
        assert_eq!(sanitize_file_name("normal.txt"), "normal.txt");
    }

    #[test]
    fn test_unique_dest_path_avoids_collisions() {
        let dir = std::env::temp_dir().join(format!("kvm_unique_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let first = unique_dest_path(&dir, "file.txt");
        assert_eq!(first, dir.join("file.txt"));
        std::fs::write(&first, b"a").unwrap();

        let second = unique_dest_path(&dir, "file.txt");
        assert_eq!(second, dir.join("file (1).txt"));
        std::fs::write(&second, b"b").unwrap();

        let third = unique_dest_path(&dir, "file.txt");
        assert_eq!(third, dir.join("file (2).txt"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_unique_dest_path_no_extension() {
        let dir = std::env::temp_dir().join(format!("kvm_unique_noext_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README"), b"a").unwrap();

        let next = unique_dest_path(&dir, "README");
        assert_eq!(next, dir.join("README (1)"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
