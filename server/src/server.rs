use crate::capture::InputCapture;
use crate::config::Config;
use crate::file_transfer::FileTransferServer;
use crate::tls;
use anyhow::Result;
use kvm_common::{deserialize_envelope, serialize_envelope, Caps, Envelope, PROTOCOL_VERSION};
use rustls::ServerConfig;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn};

pub async fn run_server(
    port: u16,
    file_port: u16,
    tls_config: Arc<ServerConfig>,
    config: Arc<RwLock<Config>>,
) -> Result<()> {
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    info!("Server listening on {}", addr);

    let acceptor = TlsAcceptor::from(tls_config);

    // Start file transfer server
    let file_config = config.clone();
    tokio::spawn(async move {
        if let Err(e) = FileTransferServer::run(file_port, file_config).await {
            error!("File transfer server error: {}", e);
        }
    });

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("New connection from {}", addr);
                let acceptor = acceptor.clone();
                let config = config.clone();

                tokio::spawn(async move {
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            if let Err(e) = handle_client(tls_stream, config).await {
                                error!("Error handling client {}: {}", addr, e);
                            }
                        }
                        Err(e) => {
                            error!("TLS handshake failed for {}: {}", addr, e);
                        }
                    }
                });
            }
            Err(e) => {
                error!("Failed to accept connection: {}", e);
            }
        }
    }
}

async fn handle_client<S>(
    mut stream: S,
    config: Arc<RwLock<Config>>,
) -> Result<()>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    // Handshake
    let hello = read_envelope(&mut stream).await?;

    match hello {
        Envelope::Hello { name, version, caps } => {
            info!("Client {} connected (version: {})", name, version);
            if version != PROTOCOL_VERSION {
                warn!("Protocol version mismatch: {} vs {}", version, PROTOCOL_VERSION);
            }

            send_envelope(&mut stream, &Envelope::HelloAck).await?;
        }
        _ => {
            send_envelope(&mut stream, &Envelope::Error("Expected Hello".to_string())).await?;
            anyhow::bail!("Expected Hello message");
        }
    }

    // Pairing
    let pair_req = read_envelope(&mut stream).await?;

    match pair_req {
        Envelope::PairRequest { nonce, fingerprint } => {
            let cfg = config.read().await;
            let is_paired = cfg.is_paired(&fingerprint);
            drop(cfg);

            if is_paired {
                info!("Client already paired: {}", fingerprint);
                send_envelope(&mut stream, &Envelope::PairAccept).await?;
            } else {
                // In CLI mode, auto-accept for now
                // In GUI mode, this would show a dialog
                info!("New pairing request from fingerprint: {}", fingerprint);

                let mut cfg = config.write().await;
                cfg.add_paired_device(fingerprint.clone(), "Unknown".to_string());
                drop(cfg);

                send_envelope(&mut stream, &Envelope::PairAccept).await?;
                info!("Pairing accepted");
            }
        }
        _ => {
            send_envelope(&mut stream, &Envelope::Error("Expected PairRequest".to_string())).await?;
            anyhow::bail!("Expected PairRequest message");
        }
    }

    // Set up input capture
    let capture = Arc::new(InputCapture::new());
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    capture.start(event_tx);

    // Spawn task to send captured events to client
    let capture_clone = capture.clone();
    let mut write_half = stream;

    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let envelope = Envelope::Input(event);
            if let Err(e) = send_envelope(&mut write_half, &envelope).await {
                error!("Failed to send event: {}", e);
                break;
            }
        }
    });

    // Main message loop (simplified for now)
    // In full implementation, we'd handle incoming messages from client
    // For now, just keep connection alive
    tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;

    Ok(())
}

async fn read_envelope<S>(stream: &mut S) -> Result<Envelope>
where
    S: AsyncReadExt + Unpin,
{
    // Read length prefix (u32 big-endian)
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > 10 * 1024 * 1024 {
        anyhow::bail!("Message too large: {} bytes", len);
    }

    // Read message data
    let mut data = vec![0u8; len];
    stream.read_exact(&mut data).await?;

    deserialize_envelope(&data).map_err(Into::into)
}

async fn send_envelope<S>(stream: &mut S, envelope: &Envelope) -> Result<()>
where
    S: AsyncWriteExt + Unpin,
{
    let data = serialize_envelope(envelope)?;
    stream.write_all(&data).await?;
    stream.flush().await?;
    Ok(())
}
