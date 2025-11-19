use crate::capture::InputCapture;
use crate::config::Config;
use crate::file_transfer::FileTransferServer;
use crate::tls;
use anyhow::Result;
use kvm_common::{read_envelope, send_envelope, Caps, Envelope, PROTOCOL_VERSION};
use rustls::ServerConfig;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn};
use arboard::Clipboard;

pub async fn run_server(
    port: u16,
    file_port: u16,
    tls_config: Arc<ServerConfig>,
    config: Arc<RwLock<Config>>,
    forwarding_enabled: Arc<AtomicBool>,
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
                let forwarding_enabled = forwarding_enabled.clone();

                tokio::spawn(async move {
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            if let Err(e) =
                                handle_client(tls_stream, config, forwarding_enabled, file_port)
                                    .await
                            {
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
    forwarding_enabled: Arc<AtomicBool>,
    file_port: u16,
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

            send_envelope(&mut stream, &Envelope::HelloAck { file_transfer_port: file_port }).await?;
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

    let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();

    // Split the stream for concurrent reads and writes
    let (mut read_half, mut write_half) = tokio::io::split(stream);

    // Task to read incoming messages from the client
    tokio::spawn(async move {
        loop {
            match read_envelope(&mut read_half).await {
                Ok(envelope) => {
                    if tx.send(envelope).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Task to send captured events to client
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let envelope = Envelope::Input(event);
            if let Err(e) = send_envelope(&mut write_half, &envelope).await {
                error!("Failed to send event: {}", e);
                break;
            }
        }
    });

    let mut clipboard = Clipboard::new()?;
    let mut last_clipboard = clipboard.get_text().unwrap_or_default();
    let mut clipboard_interval = tokio::time::interval(tokio::time::Duration::from_secs(1));

    loop {
        tokio::select! {
            Some(envelope) = rx.recv() => {
                match envelope {
                    Envelope::Goodbye => {
                        info!("Client disconnected");
                        break;
                    }
                    _ => {
                        warn!("Unhandled message: {:?}", envelope);
                    }
                }
            }
            _ = clipboard_interval.tick() => {
                if let Ok(current_clipboard) = clipboard.get_text() {
                    if current_clipboard != last_clipboard {
                        last_clipboard = current_clipboard.clone();
                        let envelope = Envelope::ClipboardSet {
                            text: current_clipboard,
                        };
                        if send_envelope(&mut write_half, &envelope).await.is_err() {
                            error!("Failed to send clipboard update");
                            break;
                        }
                    }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                let is_forwarding = forwarding_enabled.load(Ordering::SeqCst);
                if is_forwarding != capture.is_enabled() {
                    capture.set_enabled(is_forwarding);
                    debug!("Set forwarding to {}", is_forwarding);
                }
            }
        }
    }
    Ok(())
}
