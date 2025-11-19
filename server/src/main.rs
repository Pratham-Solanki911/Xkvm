mod capture;
mod config;
mod discovery;
mod file_transfer;
mod hotkey;
mod server;
mod tls;

use anyhow::Result;
use clap::Parser;
use config::Config;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyManager, HotKeyState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "kvm-server")]
#[command(about = "KVM Server - Share keyboard, mouse, files, and clipboard", long_about = None)]
struct Args {
    /// Server listening port
    #[arg(short, long, default_value_t = kvm_common::DEFAULT_CONTROL_PORT)]
    port: u16,

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

    // Shared state for input forwarding
    let forwarding_enabled = Arc::new(AtomicBool::new(false));

    // Set up global hotkey for toggling
    let hotkey_manager = GlobalHotKeyManager::new()?;
    let hotkey_str = {
        let cfg = config.read().await;
        cfg.hotkey.toggle_forward.clone()
    };
    let hotkey = hotkey::parse_hotkey(&hotkey_str)?;
    hotkey_manager.register(hotkey)?;
    info!("Registered hotkey: {} to toggle input forwarding", hotkey_str);

    let hotkey_receiver = hotkey_manager.get_receiver();
    let forwarding_clone = forwarding_enabled.clone();

    // Spawn a thread to listen for hotkey events
    std::thread::spawn(move || {
        while let Ok(event) = hotkey_receiver.recv() {
            if event.state == HotKeyState::Pressed {
                let new_state = !forwarding_clone.load(Ordering::SeqCst);
                forwarding_clone.store(new_state, Ordering::SeqCst);
                if new_state {
                    info!("Input forwarding ENABLED");
                } else {
                    info!("Input forwarding DISABLED");
                }
            }
        }
    });

    // Start the server
    let file_port = {
        let cfg = config.read().await;
        cfg.file_transfer_port
    };
    server::run_server(
        args.port,
        file_port,
        tls_config,
        config,
        forwarding_enabled,
    )
    .await?;

    Ok(())
}
