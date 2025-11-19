use crate::config::Config;
use anyhow::Result;
use kvm_common::{read_envelope, send_envelope, Envelope};
use std::sync::Arc;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use sha2::{Sha256, Digest};

pub struct FileTransferServer;

impl FileTransferServer {
    pub async fn run(port: u16, config: Arc<RwLock<Config>>) -> Result<()> {
        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr).await?;
        info!("File transfer server listening on {}", addr);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("File transfer connection from {}", addr);
                    let config = config.clone();

                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_transfer(stream, config).await {
                            error!("File transfer error from {}: {}", addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept file transfer connection: {}", e);
                }
            }
        }
    }

    async fn handle_transfer(
        mut stream: tokio::net::TcpStream,
        config: Arc<RwLock<Config>>,
    ) -> Result<()> {
        let offer = read_envelope(&mut stream).await?;

        if let Envelope::FileOffer {
            id,
            name,
            size,
            sha256,
        } = offer
        {
            info!("Receiving file '{}' ({} bytes)", name, size);

            let downloads_dir = dirs::download_dir().unwrap_or_else(|| ".".into());
            let dest_path = downloads_dir.join(&name);

            let (mut file, start_offset) = if dest_path.exists() {
                let existing_size = tokio::fs::metadata(&dest_path).await?.len();
                if existing_size < size {
                    info!("Resuming download for '{}' at offset {}", name, existing_size);
                    let file = OpenOptions::new().append(true).open(&dest_path).await?;
                    (file, existing_size)
                } else {
                    info!("File '{}' already exists, overwriting", name);
                    (File::create(&dest_path).await?, 0)
                }
            } else {
                (File::create(&dest_path).await?, 0)
            };

            send_envelope(
                &mut stream,
                &Envelope::FileAccept { id, start_offset },
            )
            .await?;

            let mut received_bytes = 0;
            while received_bytes < size {
                let mut chunk = vec![0; 65536];
                let bytes_read = stream.read(&mut chunk).await?;
                if bytes_read == 0 {
                    break;
                }

                file.write_all(&chunk[..bytes_read]).await?;
                received_bytes += bytes_read as u64;
            }

            info!(
                "File '{}' received successfully ({} bytes)",
                name, received_bytes
            );

            // Verify SHA256 checksum
            let mut file = File::open(&dest_path).await?;
            let mut hasher = Sha256::new();
            let mut buffer = [0; 65536];
            loop {
                let bytes_read = file.read(&mut buffer).await?;
                if bytes_read == 0 {
                    break;
                }
                hasher.update(&buffer[..bytes_read]);
            }
            let calculated_sha256 = hasher.finalize();

            if calculated_sha256.as_slice() == sha256.as_slice() {
                info!("SHA256 checksum verified for '{}'", name);
            } else {
                warn!("SHA256 checksum mismatch for '{}'", name);
                tokio::fs::remove_file(&dest_path).await?;
                let _ = send_envelope(&mut stream, &Envelope::Error("Checksum mismatch".to_string())).await;
            }
        } else {
            anyhow::bail!("Expected FileOffer");
        }

        Ok(())
    }
}
