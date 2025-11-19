mod config;
mod discovery;
mod file_transfer;
mod inject;

use anyhow::Result;
use clap::Parser;
use config::Config;
use kvm_common::{read_envelope, send_envelope, Caps, Envelope, PROTOCOL_VERSION};
use rustls::ClientConfig;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "kvm-client")]
#[command(about = "KVM Client - Receive keyboard, mouse, files, and clipboard", long_about = None)]
struct Args {
    /// Server address (IP:PORT or just IP)
    #[arg(short, long)]
    server: Option<String>,

    /// Server port (if not specified in --server)
    #[arg(short, long, default_value_t = kvm_common::DEFAULT_CONTROL_PORT)]
    port: u16,

    /// Auto-discover server via mDNS
    #[arg(long)]
    discover: bool,

    /// Configuration file path
    #[arg(short, long)]
    config: Option<String>,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// File to send
    #[arg(long)]
    send_file: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let log_level = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("kvm_client={},kvm_common={}", log_level, log_level).into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("KVM Client v{} starting...", kvm_common::PROTOCOL_VERSION);

    // Determine server address
    let server_addr = if let Some(server) = args.server {
        if server.contains(':') {
            server
        } else {
            format!("{}:{}", server, args.port)
        }
    } else if args.discover {
        info!("Discovering servers via mDNS...");
        match discovery::discover_servers().await {
            Ok(servers) if !servers.is_empty() => {
                let server = &servers[0];
                info!("Found server: {} at {}", server.name, server.address);
                server.address.clone()
            }
            Ok(_) => {
                error!("No servers found");
                return Ok(());
            }
            Err(e) => {
                error!("Discovery failed: {}", e);
                return Ok(());
            }
        }
    } else {
        error!("Please specify --server <address> or use --discover");
        return Ok(());
    };

    // Connect to server
    info!("Connecting to server at {}...", server_addr);
    let (tls_stream, file_transfer_port) = connect_to_server(&server_addr).await?;

    if let Some(file_path) = args.send_file {
        let file_transfer_addr = server_addr.replace(
            &args.port.to_string(),
            &file_transfer_port.to_string(),
        );
        info!("Sending file '{}' to {}", file_path, file_transfer_addr);
        file_transfer::send_file(&file_transfer_addr, &std::path::PathBuf::from(file_path)).await?;
    } else {
        run_message_loop(tls_stream).await?;
    }

    Ok(())
}

use tokio_rustls::client::TlsStream;

async fn connect_to_server(
    addr: &str,
) -> Result<(TlsStream<TcpStream>, u16)> {
    // Set up TLS
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let mut config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    // Accept self-signed certificates (for development)
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(AcceptAnyCertVerifier));

    let connector = TlsConnector::from(Arc::new(config));

    // Connect
    let stream = TcpStream::connect(addr).await?;
    let domain = rustls::pki_types::ServerName::try_from("kvm-server")
        .map_err(|_| anyhow::anyhow!("Invalid DNS name"))?
        .to_owned();

    let mut tls_stream = connector.connect(domain, stream).await?;

    info!("Connected to server");

    // Send Hello
    let hostname = hostname::get()?
        .into_string()
        .unwrap_or_else(|_| "kvm-client".to_string());

    send_envelope(
        &mut tls_stream,
        &Envelope::Hello {
            name: hostname,
            version: PROTOCOL_VERSION.to_string(),
            caps: Caps::default(),
        },
    )
    .await?;

    // Receive HelloAck
    let ack = read_envelope(&mut tls_stream).await?;
    let file_transfer_port = match ack {
        Envelope::HelloAck { file_transfer_port } => {
            info!("Handshake complete");
            file_transfer_port
        }
        Envelope::Error(msg) => {
            anyhow::bail!("Server error: {}", msg);
        }
        _ => {
            anyhow::bail!("Unexpected response: {:?}", ack);
        }
    }

    // Send pairing request
    use sha2::{Digest, Sha256};
    let nonce: [u8; 16] = rand::random();
    let mut hasher = Sha256::new();
    hasher.update(&nonce);
    let fingerprint = format!("{:x}", hasher.finalize());

    send_envelope(
        &mut tls_stream,
        &Envelope::PairRequest {
            nonce,
            fingerprint: fingerprint.clone(),
        },
    )
    .await?;

    // Receive pairing response
    let pair_response = read_envelope(&mut tls_stream).await?;
    match pair_response {
        Envelope::PairAccept => {
            info!("Pairing accepted");
        }
        Envelope::PairReject { reason } => {
            anyhow::bail!("Pairing rejected: {}", reason);
        }
        _ => {
            anyhow::bail!("Unexpected pairing response: {:?}", pair_response);
        }
    }

    Ok((tls_stream, file_transfer_port))
}

async fn run_message_loop(mut tls_stream: TlsStream<TcpStream>) -> Result<()> {
    // Start input injection
    let injector = inject::InputInjector::new();

    // Main message loop
    loop {
        let envelope = read_envelope(&mut tls_stream).await?;

        match envelope {
            Envelope::Input(event) => {
                if let Err(e) = injector.inject(event) {
                    error!("Failed to inject event: {}", e);
                }
            }
            Envelope::ClipboardSet { text } => {
                info!("Received clipboard: {} bytes", text.len());
                let mut clipboard = arboard::Clipboard::new()?;
                clipboard.set_text(text)?;
            }
            Envelope::Ping(ts) => {
                send_envelope(&mut tls_stream, &Envelope::Pong(ts)).await?;
            }
            Envelope::Goodbye => {
                info!("Server disconnected");
                break;
            }
            Envelope::Error(msg) => {
                error!("Server error: {}", msg);
            }
            _ => {
                warn!("Unexpected message: {:?}", envelope);
            }
        }
    }

    Ok(())
}

// Certificate verifier that accepts any certificate (for self-signed certs)
struct AcceptAnyCertVerifier;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer,
        _intermediates: &[rustls::pki_types::CertificateDer],
        _server_name: &rustls::pki_types::ServerName,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ED25519,
        ]
    }
}
