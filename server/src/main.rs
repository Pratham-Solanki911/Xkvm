mod capture;
mod config;
mod discovery;
mod file_transfer;
mod server;
mod tls;

use anyhow::Result;
use clap::Parser;
use config::Config;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "kvm-server")]
#[command(about = "KVM Server - Share keyboard, mouse, files, and clipboard", long_about = None)]
struct Args {
    /// Server listening port
    #[arg(short, long, default_value_t = kvm_common::DEFAULT_CONTROL_PORT)]
    port: u16,

    /// File transfer port
    #[arg(long, default_value_t = kvm_common::DEFAULT_FILE_PORT)]
    file_port: u16,

    /// Disable mDNS discovery
    #[arg(long)]
    no_mdns: bool,

    /// Configuration file path
    #[arg(short, long)]
    config: Option<String>,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let log_level = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("kvm_server={},kvm_common={}", log_level, log_level).into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("KVM Server v{} starting...", kvm_common::PROTOCOL_VERSION);

    // Load or create configuration
    let config = if let Some(config_path) = &args.config {
        Config::load(config_path)?
    } else {
        Config::default()
    };

    let config = Arc::new(RwLock::new(config));

    // Generate or load TLS certificates
    let tls_config = tls::setup_tls_server()?;
    info!("TLS certificate loaded/generated");

    // Start mDNS discovery service
    let _mdns_service = if !args.no_mdns {
        match discovery::start_mdns_server(args.port).await {
            Ok(service) => {
                info!("mDNS service started");
                Some(service)
            }
            Err(e) => {
                error!("Failed to start mDNS service: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Start the server
    server::run_server(args.port, args.file_port, tls_config, config).await?;

    Ok(())
}
