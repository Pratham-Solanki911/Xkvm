use crate::capture::{CaptureEvent, InputCapture};
use crate::config::Config;
use crate::file_transfer::FileTransferServer;
use crate::hotkey;
use anyhow::Result;
use arboard::Clipboard;
use kvm_common::{read_envelope, send_envelope, Envelope, Event, PROTOCOL_VERSION};
use rustls::ServerConfig;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn};

/// Clipboard payloads larger than this are ignored in both directions.
const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
/// How long a peer IP stays blocked after too many failed pairing proofs.
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(600);
/// How many failed proofs within the window trigger a block.
const RATE_LIMIT_MAX_FAILURES: usize = 5;
/// Deadline for the TLS handshake and for each pre-pairing read (`Hello`,
/// `PairRequest`). Without this, a peer that opens a TCP connection and
/// simply never sends anything ties up a task (and, for the handshake, a
/// socket) forever at zero cost to the attacker.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Caps how many connections may be mid-handshake/mid-pairing at once, so an
/// attacker opening many silent connections can't grow the task/socket count
/// without bound. Once a connection finishes pairing its permit is released,
/// so this does not limit the number of paired, steady-state clients.
const MAX_PENDING_CONNECTIONS: usize = 256;

/// Fan-out registry of currently paired, connected clients: connection id ->
/// sender for that connection's outbound (server -> client) envelopes.
pub type Registry = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<Envelope>>>>;

#[derive(Debug, Default)]
pub struct RateLimitEntry {
    pub failures: Vec<Instant>,
    pub blocked_until: Option<Instant>,
}

/// Shared state every connection handler needs. Kept as one `Clone`-able
/// struct (everything inside is already `Arc`/cheaply cloned) so
/// `handle_client` can be driven directly from tests over `tokio::io::duplex`
/// without spinning up a real listener.
#[derive(Clone)]
pub struct ServerState {
    pub config: Arc<RwLock<Config>>,
    pub config_path: Arc<PathBuf>,
    pub forwarding_enabled: Arc<AtomicBool>,
    pub registry: Registry,
    pub file_port: u16,
    pub pin: Arc<String>,
    pub rate_limiter: Arc<Mutex<HashMap<IpAddr, RateLimitEntry>>>,
    /// `None` when clipboard sync is disabled or the local clipboard
    /// couldn't be opened; `Some` sends text for the watcher thread to apply
    /// to the local clipboard (and remember, to avoid echoing it back).
    pub clipboard_tx: Option<std::sync::mpsc::Sender<String>>,
}

// Startup wiring for the whole server; grouping these into a struct would
// just move the same fields around without adding clarity at the single
// call site in `main.rs`.
#[allow(clippy::too_many_arguments)]
pub async fn run_server(
    port: u16,
    file_port: u16,
    bind_address: String,
    tls_config: Arc<ServerConfig>,
    config: Arc<RwLock<Config>>,
    config_path: PathBuf,
    forwarding_enabled: Arc<AtomicBool>,
    pin: String,
) -> Result<()> {
    let addr = format!("{}:{}", bind_address, port);
    let listener = TcpListener::bind(&addr).await?;
    info!("Server listening on {}", addr);

    let acceptor = TlsAcceptor::from(tls_config.clone());

    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicU64::new(1));
    let rate_limiter = Arc::new(Mutex::new(HashMap::new()));
    let pending_connections = Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_CONNECTIONS));

    let clipboard_sync = { config.read().await.clipboard_sync };
    let clipboard_tx = if clipboard_sync {
        spawn_clipboard_watcher(registry.clone())
    } else {
        info!("Clipboard sync disabled by config");
        None
    };

    let state = ServerState {
        config: config.clone(),
        config_path: Arc::new(config_path),
        forwarding_enabled: forwarding_enabled.clone(),
        registry: registry.clone(),
        file_port,
        pin: Arc::new(pin),
        rate_limiter,
        clipboard_tx,
    };

    // File transfer server, TLS-wrapped with the same certificate.
    {
        let file_config = config.clone();
        let file_bind = bind_address.clone();
        let file_tls = tls_config;
        tokio::spawn(async move {
            if let Err(e) =
                FileTransferServer::run(&file_bind, file_port, file_tls, file_config).await
            {
                error!("File transfer server error: {}", e);
            }
        });
    }

    // A single global input capture runs for the whole server lifetime; it
    // is never spun up per-connection.
    let capture = Arc::new(InputCapture::new(forwarding_enabled.clone()));
    let (capture_tx, mut capture_rx) = mpsc::unbounded_channel::<CaptureEvent>();
    let hotkey_str = { config.read().await.hotkey.toggle_forward.clone() };
    let hotkey = match hotkey::parse_hotkey(&hotkey_str) {
        Ok(hk) => {
            info!(
                "Registered hotkey '{}' to toggle input forwarding",
                hotkey_str
            );
            Some(hk)
        }
        Err(e) => {
            warn!(
                "Failed to parse hotkey '{}': {} (hotkey toggling disabled; use ToggleForward \
                 from a paired client instead)",
                hotkey_str, e
            );
            None
        }
    };
    capture.start(capture_tx, hotkey);

    {
        let registry = registry.clone();
        tokio::spawn(async move {
            while let Some(msg) = capture_rx.recv().await {
                match msg {
                    CaptureEvent::Input(ev) => {
                        broadcast(&registry, Envelope::Input(ev));
                    }
                    CaptureEvent::ForwardingToggled(new_state) => {
                        info!(target: "audit", "input forwarding toggled via hotkey: {}", new_state);
                        if !new_state {
                            broadcast(&registry, Envelope::Input(Event::ReleaseAll));
                        }
                        broadcast(&registry, Envelope::ToggleForward(new_state));
                    }
                }
            }
        });
    }

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, peer_addr)) => {
                        let permit = match pending_connections.clone().try_acquire_owned() {
                            Ok(permit) => permit,
                            Err(_) => {
                                warn!(
                                    "Too many pending (unhandshaked/unpaired) connections; \
                                     dropping new connection from {}",
                                    peer_addr
                                );
                                continue;
                            }
                        };
                        info!("New connection from {}", peer_addr);
                        let _ = stream.set_nodelay(true);
                        let acceptor = acceptor.clone();
                        let state = state.clone();
                        let client_id = next_id.fetch_add(1, Ordering::SeqCst);

                        tokio::spawn(async move {
                            match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                                Ok(Ok(tls_stream)) => {
                                    if let Err(e) = handle_client(
                                        tls_stream,
                                        state,
                                        peer_addr.ip(),
                                        client_id,
                                        permit,
                                    )
                                    .await
                                    {
                                        error!("Error handling client {}: {}", peer_addr, e);
                                    }
                                }
                                Ok(Err(e)) => {
                                    error!("TLS handshake failed for {}: {}", peer_addr, e);
                                }
                                Err(_) => {
                                    warn!("TLS handshake timed out for {}", peer_addr);
                                }
                            }
                            // `permit` is dropped here in every branch,
                            // releasing the pending-connection slot.
                        });
                    }
                    Err(e) => {
                        error!("Failed to accept connection: {}", e);
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Shutdown signal received, disconnecting clients...");
                broadcast(&registry, Envelope::Goodbye);
                break;
            }
        }
    }

    Ok(())
}

fn broadcast(registry: &Registry, envelope: Envelope) {
    let map = registry.lock().unwrap();
    for tx in map.values() {
        let _ = tx.send(envelope.clone());
    }
}

/// Starts the single clipboard-owning watcher thread (arboard's `Clipboard`
/// is not meant to be shared across threads). Returns `None` if the local
/// clipboard couldn't be opened at all, after logging once.
fn spawn_clipboard_watcher(registry: Registry) -> Option<std::sync::mpsc::Sender<String>> {
    let mut clipboard = match Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            error!(
                "Failed to initialize clipboard: {} (clipboard sync disabled)",
                e
            );
            return None;
        }
    };

    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<String>();

    std::thread::spawn(move || {
        let mut last_seen = clipboard.get_text().unwrap_or_default();
        loop {
            while let Ok(text) = cmd_rx.try_recv() {
                if text != last_seen && text.len() <= MAX_CLIPBOARD_BYTES {
                    last_seen = text.clone();
                    let _ = clipboard.set_text(text);
                }
            }

            if let Ok(current) = clipboard.get_text() {
                if current != last_seen
                    && !current.is_empty()
                    && current.len() <= MAX_CLIPBOARD_BYTES
                {
                    last_seen = current.clone();
                    broadcast(&registry, Envelope::ClipboardSet { text: current });
                }
            }

            std::thread::sleep(Duration::from_millis(500));
        }
    });

    Some(cmd_tx)
}

/// Shared with `socks5.rs`, which mirrors this same per-IP lockout for
/// SOCKS5 authentication failures.
pub(crate) fn is_rate_limited(
    map: &Arc<Mutex<HashMap<IpAddr, RateLimitEntry>>>,
    ip: IpAddr,
) -> bool {
    let guard = map.lock().unwrap();
    if let Some(entry) = guard.get(&ip) {
        if let Some(until) = entry.blocked_until {
            return Instant::now() < until;
        }
    }
    false
}

pub(crate) fn record_pairing_failure(
    map: &Arc<Mutex<HashMap<IpAddr, RateLimitEntry>>>,
    ip: IpAddr,
) {
    let mut guard = map.lock().unwrap();
    let now = Instant::now();
    let entry = guard.entry(ip).or_insert_with(|| RateLimitEntry {
        failures: Vec::new(),
        blocked_until: None,
    });
    entry
        .failures
        .retain(|t| now.duration_since(*t) < RATE_LIMIT_WINDOW);
    entry.failures.push(now);
    if entry.failures.len() >= RATE_LIMIT_MAX_FAILURES {
        entry.blocked_until = Some(now + RATE_LIMIT_WINDOW);
    }
}

/// Once a device is paired, its fingerprint is a de-facto bearer credential
/// (the "already paired" path accepts any proof for a known fingerprint) -
/// so full fingerprints are kept out of logs that a bystander could plausibly
/// read (info/warn level, and anything that reaches the audit-log file).
/// Enough of a prefix survives for an admin to eyeball during pairing; the
/// full value is still available at `debug` level for troubleshooting.
fn redact_fingerprint(fingerprint: &str) -> String {
    let visible: String = fingerprint.chars().take(11).collect();
    format!("{}...", visible)
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub async fn handle_client<S>(
    mut stream: S,
    state: ServerState,
    peer_ip: IpAddr,
    client_id: u64,
    pending_permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // --- Handshake ---
    // Everything up to a successful pairing is bounded by both a deadline
    // and `pending_permit` (released once pairing succeeds below) - an
    // unauthenticated peer that never sends anything can't tie up this task
    // forever, nor can unlimited such peers exhaust the connection pool.
    let hello = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_envelope(&mut stream))
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for Hello from {}", peer_ip))??;
    let client_name = match hello {
        Envelope::Hello {
            name,
            version,
            caps,
        } => {
            if version != PROTOCOL_VERSION {
                let _ = send_envelope(
                    &mut stream,
                    &Envelope::Error(format!(
                        "protocol version mismatch: server={}, client={}",
                        PROTOCOL_VERSION, version
                    )),
                )
                .await;
                anyhow::bail!(
                    "protocol version mismatch from {} ('{}'): {}",
                    peer_ip,
                    name,
                    version
                );
            }
            debug!("Client '{}' caps: {:?}", name, caps);
            name
        }
        _ => {
            let _ =
                send_envelope(&mut stream, &Envelope::Error("expected Hello".to_string())).await;
            anyhow::bail!("expected Hello message from {}", peer_ip);
        }
    };

    let server_name = { state.config.read().await.server_name.clone() };
    send_envelope(
        &mut stream,
        &Envelope::HelloAck {
            file_transfer_port: state.file_port,
            server_name,
        },
    )
    .await?;

    // --- Pairing ---
    let pair_req = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_envelope(&mut stream))
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for PairRequest from {}", peer_ip))??;
    let fingerprint = match pair_req {
        Envelope::PairRequest {
            nonce,
            fingerprint,
            proof,
        } => {
            if is_rate_limited(&state.rate_limiter, peer_ip) {
                warn!(target: "audit", "rejected pairing from rate-limited peer {}", peer_ip);
                send_envelope(
                    &mut stream,
                    &Envelope::PairReject {
                        reason: "too many failed pairing attempts; try again later".to_string(),
                    },
                )
                .await?;
                anyhow::bail!("peer {} is rate-limited", peer_ip);
            }

            let already_paired = { state.config.read().await.is_paired(&fingerprint) };
            if already_paired {
                info!(target: "audit", "'{}' ({}) re-connected (already paired)", client_name, redact_fingerprint(&fingerprint));
                debug!("full fingerprint for '{}': {}", client_name, fingerprint);
                send_envelope(&mut stream, &Envelope::PairAccept).await?;
            } else {
                let expected = kvm_common::pairing_proof(&state.pin, &nonce, &fingerprint);
                if kvm_common::proof_matches(&expected, &proof) {
                    {
                        let mut cfg = state.config.write().await;
                        cfg.add_paired_device(fingerprint.clone(), client_name.clone());
                        if let Err(e) = cfg.save(&state.config_path) {
                            warn!("Failed to persist config after pairing: {}", e);
                        }
                    }
                    info!(target: "audit", "paired new device '{}' ({}) from {}", client_name, redact_fingerprint(&fingerprint), peer_ip);
                    debug!("full fingerprint for '{}': {}", client_name, fingerprint);
                    send_envelope(&mut stream, &Envelope::PairAccept).await?;
                } else {
                    record_pairing_failure(&state.rate_limiter, peer_ip);
                    warn!(target: "audit", "rejected pairing from {} (bad PIN, fingerprint {})", peer_ip, redact_fingerprint(&fingerprint));
                    send_envelope(
                        &mut stream,
                        &Envelope::PairReject {
                            reason: "pairing PIN required or incorrect".to_string(),
                        },
                    )
                    .await?;
                    anyhow::bail!("pairing rejected for {} ({})", peer_ip, fingerprint);
                }
            }
            fingerprint
        }
        _ => {
            send_envelope(
                &mut stream,
                &Envelope::Error("expected PairRequest".to_string()),
            )
            .await?;
            anyhow::bail!("expected PairRequest message from {}", peer_ip);
        }
    };

    // --- Register for broadcast, honor auto_forward ---
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Envelope>();
    let is_first_client = {
        let mut map = state.registry.lock().unwrap();
        map.insert(client_id, outbound_tx.clone());
        map.len() == 1
    };

    if is_first_client {
        let auto_forward = { state.config.read().await.auto_forward };
        if auto_forward && !state.forwarding_enabled.swap(true, Ordering::SeqCst) {
            info!(target: "audit", "auto_forward: forwarding turned ON (first client paired)");
            let _ = outbound_tx.send(Envelope::ToggleForward(true));
        }
    }

    // Pairing succeeded and this connection is now registered; it no longer
    // needs to count against the pending (unauthenticated) connection cap,
    // which exists only to bound handshake/pairing-in-progress connections.
    drop(pending_permit);

    let (mut read_half, mut write_half) = tokio::io::split(stream);

    tokio::spawn(async move {
        while let Some(envelope) = outbound_rx.recv().await {
            if let Err(e) = send_envelope(&mut write_half, &envelope).await {
                error!("Failed to send envelope: {}", e);
                break;
            }
        }
    });

    let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<Envelope>();
    tokio::spawn(async move {
        while let Ok(envelope) = read_envelope(&mut read_half).await {
            if inbound_tx.send(envelope).is_err() {
                break;
            }
        }
    });

    let mut ping_interval = tokio::time::interval(Duration::from_secs(5));
    ping_interval.tick().await; // consume the immediate first tick
    let mut watchdog = tokio::time::interval(Duration::from_secs(1));
    let mut last_seen = Instant::now();

    loop {
        tokio::select! {
            maybe_envelope = inbound_rx.recv() => {
                match maybe_envelope {
                    Some(envelope) => {
                        last_seen = Instant::now();
                        match envelope {
                            Envelope::Goodbye => {
                                info!("Client '{}' sent Goodbye", client_name);
                                break;
                            }
                            Envelope::Ping(ts) => {
                                let _ = outbound_tx.send(Envelope::Pong(ts));
                            }
                            Envelope::Pong(ts) => {
                                debug!(
                                    "RTT to '{}': {} ms",
                                    client_name,
                                    unix_millis().saturating_sub(ts)
                                );
                            }
                            Envelope::ToggleForward(desired) => {
                                let current = state.forwarding_enabled.load(Ordering::SeqCst);
                                if desired != current {
                                    state.forwarding_enabled.store(desired, Ordering::SeqCst);
                                    info!(target: "audit", "forwarding toggled by client '{}': {}", client_name, desired);
                                    if !desired {
                                        broadcast(&state.registry, Envelope::Input(Event::ReleaseAll));
                                    }
                                    broadcast(&state.registry, Envelope::ToggleForward(desired));
                                }
                            }
                            Envelope::ClipboardSet { text } => {
                                let clipboard_sync = { state.config.read().await.clipboard_sync };
                                if clipboard_sync && text.len() <= MAX_CLIPBOARD_BYTES {
                                    if let Some(tx) = &state.clipboard_tx {
                                        let _ = tx.send(text);
                                    }
                                }
                            }
                            other => {
                                warn!("Unhandled message from '{}': {:?}", client_name, other);
                            }
                        }
                    }
                    None => {
                        info!("Client '{}' connection closed", client_name);
                        break;
                    }
                }
            }
            _ = ping_interval.tick() => {
                let _ = outbound_tx.send(Envelope::Ping(unix_millis()));
            }
            _ = watchdog.tick() => {
                if last_seen.elapsed() > Duration::from_secs(15) {
                    warn!("Client '{}' timed out (no activity for 15s)", client_name);
                    break;
                }
            }
        }
    }

    let now_empty = {
        let mut map = state.registry.lock().unwrap();
        map.remove(&client_id);
        map.is_empty()
    };
    if now_empty {
        let auto_forward = { state.config.read().await.auto_forward };
        if auto_forward && state.forwarding_enabled.swap(false, Ordering::SeqCst) {
            info!(target: "audit", "auto_forward: forwarding turned OFF (last client disconnected)");
        }
    }

    let _ = outbound_tx.send(Envelope::Goodbye);
    info!(target: "audit", "disconnected: '{}' ({})", client_name, redact_fingerprint(&fingerprint));

    Ok(())
}
