use anyhow::Result;
use clap::Parser;
use kvm_client::config::Config;
use kvm_client::{run_with_reconnect, ConnectOptions, Session, SessionEvent};
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "kvm-client")]
#[command(about = "KVM Client - Receive keyboard, mouse, files, and clipboard", long_about = None)]
struct Args {
    /// Server address (host:port, or just a host/IP to use --port)
    #[arg(short, long)]
    server: Option<String>,

    /// Server port (used when --server has no port of its own)
    #[arg(short, long, default_value_t = kvm_common::DEFAULT_CONTROL_PORT)]
    port: u16,

    /// Auto-discover servers via mDNS and connect to the first one found
    #[arg(long)]
    discover: bool,

    /// Configuration file path
    #[arg(short, long)]
    config: Option<String>,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// File to send to the server
    #[arg(long)]
    send_file: Option<String>,

    /// SOCKS5 proxy to route traffic through (not implemented; see warning)
    #[arg(long)]
    socks5_proxy: Option<String>,

    /// Pairing PIN (falls back to KVM_PAIRING_PIN, then empty)
    #[arg(long)]
    pin: Option<String>,

    /// Trust and pin whatever certificate the server presents right now,
    /// overwriting any previously pinned fingerprint for this address
    #[arg(long)]
    trust_new_cert: bool,

    /// Require the server's certificate fingerprint to match exactly
    /// (e.g. "AB:CD:...")
    #[arg(long)]
    fingerprint: Option<String>,

    /// Disable automatic reconnect: exit after the first disconnect or
    /// connection failure
    #[arg(long)]
    no_reconnect: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let log_level = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("kvm_client={},kvm_common={}", log_level, log_level).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("KVM Client v{} starting...", kvm_common::PROTOCOL_VERSION);

    let config_path = match &args.config {
        Some(p) => PathBuf::from(p),
        None => Config::default_path()?,
    };
    let mut config = Config::load_or_default(&config_path);

    if let Some(proxy) = &args.socks5_proxy {
        warn!(
            "--socks5-proxy ({}) does not route traffic itself: point your OS or browser's \
             SOCKS5 settings at socks5://<server-address>:<socks5-port> instead",
            proxy
        );
    }

    let server_addr = if let Some(server) = args.server {
        if server.contains(':') || server.starts_with('[') {
            server
        } else {
            format!("{}:{}", server, args.port)
        }
    } else if args.discover {
        info!("Discovering servers via mDNS...");
        let servers = kvm_client::discovery::discover_servers(Duration::from_secs(5)).await?;
        if servers.is_empty() {
            error!("No servers found");
            return Ok(());
        }
        for s in &servers {
            info!("Found server: {} at {}", s.name, s.address);
        }
        servers[0].address.clone()
    } else if let Some(last) = config.last_server.clone() {
        info!("Using last connected server: {}", last);
        last
    } else {
        error!("Please specify --server <address> or use --discover");
        return Ok(());
    };

    let opts = ConnectOptions {
        pin: args.pin,
        trust_new_cert: args.trust_new_cert,
        expected_fingerprint: args.fingerprint,
    };

    if let Some(file_path) = args.send_file {
        let session = Session::connect(&server_addr, &mut config, &opts).await?;
        if let Err(e) = config.save(&config_path) {
            warn!("failed to save client config: {}", e);
        }
        info!(
            "Sending file '{}' to {}:{}",
            file_path,
            session.host(),
            session.file_transfer_port()
        );
        kvm_client::file_transfer::send_file(
            session.host(),
            session.file_transfer_port(),
            &PathBuf::from(file_path),
            &config,
            &opts,
            None,
        )
        .await?;
        return Ok(());
    }

    let cancel = CancellationToken::new();
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                info!("Shutting down...");
                cancel.cancel();
            }
        });
    }

    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<SessionEvent>(32);
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            match event {
                SessionEvent::Connected {
                    server_name,
                    fingerprint,
                } => {
                    info!(
                        "Connected to '{}' (fingerprint {})",
                        server_name, fingerprint
                    );
                }
                SessionEvent::Disconnected { reason } => info!("Disconnected: {}", reason),
                SessionEvent::ForwardingChanged(on) => {
                    info!("Input forwarding is now {}", if on { "ON" } else { "OFF" });
                }
                SessionEvent::ClipboardReceived { bytes } => {
                    info!("Received clipboard update ({} bytes)", bytes);
                }
                SessionEvent::Latency { ms } => {
                    tracing::debug!("Latency: {} ms", ms);
                }
                SessionEvent::Error(msg) => error!("{}", msg),
            }
        }
    });

    if args.no_reconnect {
        let session = Session::connect(&server_addr, &mut config, &opts).await?;
        if let Err(e) = config.save(&config_path) {
            warn!("failed to save client config: {}", e);
        }
        session.run(cancel, events_tx).await?;
    } else {
        run_with_reconnect(server_addr, Some(config_path), opts, cancel, events_tx).await?;
    }

    Ok(())
}
