use kvm_server::*;

use anyhow::Result;
use clap::Parser;
use config::Config;
use rand::Rng;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

/// A `tracing_subscriber` writer for the audit log file that is created
/// once at startup and shared (via `Arc<Mutex<..>>`) across every emitted
/// event, rather than re-opened per event. Cloning it is just an `Arc`
/// bump - no syscall, and therefore nothing that can fail or panic on the
/// hot path; a poisoned mutex (a prior writer panicking mid-write, which
/// cannot happen here since writes are plain `io::Write` calls) degrades
/// to silently dropping the line instead of panicking the caller, since a
/// missed audit line must never be allowed to crash the server.
#[derive(Clone)]
struct SharedAuditWriter(Arc<Mutex<std::fs::File>>);

impl Write for SharedAuditWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.0.lock() {
            Ok(mut file) => file.write(buf),
            Err(_) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.0.lock() {
            Ok(mut file) => file.flush(),
            Err(_) => Ok(()),
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "kvm-server")]
#[command(about = "KVM Server - Share keyboard, mouse, files, and clipboard", long_about = None)]
struct Args {
    /// Server listening port
    #[arg(short, long, default_value_t = kvm_common::DEFAULT_CONTROL_PORT)]
    port: u16,

    /// Address to bind the control and file-transfer listeners to (overrides config)
    #[arg(long)]
    bind: Option<String>,

    /// Disable mDNS discovery
    #[arg(long)]
    no_mdns: bool,

    /// Configuration file path (default: <config_dir>/kvm-rs/server.toml)
    #[arg(short, long)]
    config: Option<String>,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Pairing PIN new devices must present (else KVM_PAIRING_PIN, else config, else a random PIN)
    #[arg(long)]
    pin: Option<String>,

    /// SOCKS5 server port
    #[arg(long)]
    socks5_port: Option<u16>,

    /// SOCKS5 username
    #[arg(long)]
    socks5_user: Option<String>,

    /// SOCKS5 password (also read from KVM_SOCKS5_PASSWORD)
    #[arg(long)]
    socks5_pass: Option<String>,

    /// Allow SOCKS5 clients to connect without authentication (open relay - use with care)
    #[arg(long)]
    socks5_allow_anonymous: bool,

    /// List paired devices and exit
    #[arg(long)]
    list_paired: bool,

    /// Revoke a device by fingerprint and exit
    #[arg(long)]
    revoke: Option<String>,

    /// Optional file path for audit logs (overrides config)
    #[arg(long)]
    audit_log: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let config_path = match &args.config {
        Some(p) => PathBuf::from(p),
        None => Config::default_path()?,
    };
    let config_existed = config_path.exists();
    let mut config = Config::load_or_default(&config_path)?;

    // Audit logging is set up before the general tracing init so both layers
    // start together; every `[AUDIT]`-worthy event below is logged with
    // `target: "audit"` and only reaches this layer.
    let audit_log_path = args
        .audit_log
        .clone()
        .map(PathBuf::from)
        .or_else(|| config.audit_log.clone());
    let audit_layer = audit_log_path.as_ref().and_then(|path| {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(file) => {
                // The audit log records pairing/rejection/revocation events,
                // including (redacted) fingerprints; restrict it like the
                // other files that carry sensitive data.
                if let Err(e) = tls::restrict_file_to_owner(path) {
                    eprintln!(
                        "Could not restrict permissions on audit log {}: {}",
                        path.display(),
                        e
                    );
                }
                let shared_writer = SharedAuditWriter(Arc::new(Mutex::new(file)));
                Some(
                    tracing_subscriber::fmt::layer()
                        .with_writer(move || shared_writer.clone())
                        .with_ansi(false)
                        .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
                            meta.target() == "audit"
                        })),
                )
            }
            Err(e) => {
                eprintln!("Failed to open audit log file {}: {}", path.display(), e);
                None
            }
        }
    });

    let log_level = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("kvm_server={},kvm_common={}", log_level, log_level).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(audit_layer)
        .init();

    info!("KVM Server v{} starting...", kvm_common::PROTOCOL_VERSION);

    if !config_existed {
        match config.save(&config_path) {
            Ok(()) => info!("Wrote default configuration to {}", config_path.display()),
            Err(e) => warn!(
                "Failed to write default configuration to {}: {}",
                config_path.display(),
                e
            ),
        }
    } else {
        info!("Loaded configuration from {}", config_path.display());
    }

    if args.list_paired {
        println!("Paired devices:");
        for (fp, info) in &config.paired_devices {
            println!(
                "- {} (Fingerprint: {}, Paired: {})",
                info.name, fp, info.paired_at
            );
        }
        return Ok(());
    }

    if let Some(fp) = args.revoke {
        if config.remove_paired_device(&fp) {
            info!(target: "audit", "revoked device: {}", fp);
            if let Err(e) = config.save(&config_path) {
                error!("Failed to save config after revoke: {}", e);
            }
        } else {
            error!("Device not found: {}", fp);
        }
        return Ok(());
    }

    // Resolve the pairing PIN: --pin, then KVM_PAIRING_PIN, then config, then
    // a random one generated (and printed) for this run only.
    let pin = args
        .pin
        .clone()
        .or_else(|| std::env::var("KVM_PAIRING_PIN").ok())
        .or_else(|| config.pairing_pin.clone())
        .unwrap_or_else(|| {
            let generated: u32 = rand::thread_rng().gen_range(0..1_000_000);
            format!("{:06}", generated)
        });
    warn!(
        "=== Pairing PIN: {} === (give this to devices you want to pair)",
        pin
    );

    let bind_address = args
        .bind
        .clone()
        .unwrap_or_else(|| config.bind_address.clone());

    // Generate or load TLS certificates
    let tls_config = tls::setup_tls_server()?;
    info!("TLS certificate loaded/generated");
    if let Ok(fingerprint) = tls::get_certificate_fingerprint() {
        info!(
            "Server certificate fingerprint (verify this on the client): {}",
            fingerprint
        );
    }

    // Start mDNS discovery service
    let server_name = config.server_name.clone();
    let _mdns_service = if !args.no_mdns {
        match discovery::start_mdns_server(args.port, &server_name).await {
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

    let socks5_username = args
        .socks5_user
        .clone()
        .or_else(|| config.socks5.username.clone());
    let socks5_password = args
        .socks5_pass
        .clone()
        .or_else(|| std::env::var("KVM_SOCKS5_PASSWORD").ok())
        .or_else(|| config.socks5.password.clone());
    let socks5_allow_anonymous = args.socks5_allow_anonymous || config.socks5.allow_anonymous;
    let socks5_port = args.socks5_port.or(config.socks5.port);

    if let Some(socks_port) = socks5_port {
        if !socks5_allow_anonymous && (socks5_username.is_none() || socks5_password.is_none()) {
            error!(
                "SOCKS5 proxy requires --socks5-user/--socks5-pass (or config/KVM_SOCKS5_PASSWORD) \
                 unless --socks5-allow-anonymous is set; not starting the proxy"
            );
        } else {
            let socks_cfg = socks5::Socks5Config {
                username: socks5_username,
                password: socks5_password,
                idle_timeout: std::time::Duration::from_secs(300),
                allow_anonymous: socks5_allow_anonymous,
            };
            let socks_bind = bind_address.clone();
            tokio::spawn(async move {
                info!(target: "audit", "starting SOCKS5 proxy on {}:{}", socks_bind, socks_port);
                if let Err(e) =
                    socks5::Socks5Server::run(&socks_bind, socks_port, Some(socks_cfg)).await
                {
                    error!("SOCKS5 server error: {}", e);
                }
            });
        }
    }

    info!(target: "audit", "server started on {}:{}", bind_address, args.port);

    let file_port = config.file_transfer_port;
    let config = Arc::new(RwLock::new(config));

    // Start the server (returns once a shutdown signal is handled).
    server::run_server(
        args.port,
        file_port,
        bind_address,
        tls_config,
        config,
        config_path,
        forwarding_enabled,
        pin,
    )
    .await?;

    Ok(())
}
