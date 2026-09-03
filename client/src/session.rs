//! Owns one control-channel connection to a server: the TLS handshake,
//! pairing, and the message loop that follows (input injection, clipboard
//! sync, keepalive, and reconnect-with-backoff).

use crate::config::Config;
use crate::inject::InputInjector;
use crate::tls::PinnedCertVerifier;
use anyhow::{Context, Result};
use kvm_common::{
    pairing_proof, read_envelope, send_envelope, Caps, Envelope, Event, PROTOCOL_VERSION,
};
use rustls::pki_types::ServerName;
use rustls::ClientConfig;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// How the caller wants a connection attempt to be trusted / authenticated.
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// Pairing PIN. Falls back to `KVM_PAIRING_PIN`, then empty (only works
    /// if this client is already paired).
    pub pin: Option<String>,
    /// Accept whatever certificate the server presents right now and pin
    /// it, overwriting any previously pinned fingerprint for this address.
    pub trust_new_cert: bool,
    /// Require this exact fingerprint (overrides both the stored pin and
    /// `trust_new_cert`).
    pub expected_fingerprint: Option<String>,
}

/// Events emitted by a running [`Session`] for a UI (or the CLI) to observe.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    Connected {
        server_name: String,
        fingerprint: String,
    },
    Disconnected {
        reason: String,
    },
    ForwardingChanged(bool),
    ClipboardReceived {
        bytes: usize,
    },
    Latency {
        ms: u64,
    },
    Error(String),
}

/// A connection failure that must not trigger a reconnect attempt: the
/// protocol version doesn't match, the PIN was rejected, or the server's
/// certificate fingerprint doesn't match what was expected.
#[derive(Debug)]
pub struct FatalConnectError(pub String);

impl std::fmt::Display for FatalConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FatalConnectError {}

/// Commands a [`SessionController`] can send into a running [`Session`].
#[derive(Debug, Clone)]
enum Command {
    ToggleForward(bool),
    SendClipboard(String),
}

/// A cheap, cloneable handle used to drive a running [`Session`] from
/// elsewhere (e.g. a Tauri command handler) without owning it.
#[derive(Clone)]
pub struct SessionController {
    tx: mpsc::UnboundedSender<Command>,
}

impl SessionController {
    pub fn toggle_forward(&self, on: bool) -> Result<()> {
        self.tx
            .send(Command::ToggleForward(on))
            .map_err(|_| anyhow::anyhow!("session is no longer running"))
    }

    pub fn send_clipboard(&self, text: String) -> Result<()> {
        self.tx
            .send(Command::SendClipboard(text))
            .map_err(|_| anyhow::anyhow!("session is no longer running"))
    }
}

/// One authenticated, paired connection to a server's control channel.
pub struct Session {
    stream: TlsStream<TcpStream>,
    host: String,
    server_name: String,
    fingerprint: String,
    file_transfer_port: u16,
    cmd_tx: mpsc::UnboundedSender<Command>,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
}

impl Session {
    /// Connect to `addr`, perform the TLS handshake (with fingerprint
    /// pinning) and pairing. On success, `config` has been updated in
    /// memory with the client's secret (generated if it didn't have one
    /// yet) and this server's entry in `known_servers` -- the caller is
    /// responsible for persisting it.
    pub async fn connect(
        addr: &str,
        config: &mut Config,
        opts: &ConnectOptions,
    ) -> Result<Session> {
        let (host, port) = crate::addr::parse_host_port(addr, kvm_common::DEFAULT_CONTROL_PORT)?;
        let normalized_addr = crate::addr::format_host_port(&host, port);

        let stored_fp = config.known_fingerprint(&normalized_addr);
        let required_fp = if let Some(fp) = &opts.expected_fingerprint {
            Some(fp.to_uppercase())
        } else if opts.trust_new_cert {
            None
        } else {
            stored_fp.clone()
        };
        let first_use = stored_fp.is_none() || opts.trust_new_cert;

        let observed = Arc::new(Mutex::new(None));
        let verifier = Arc::new(PinnedCertVerifier::new(
            required_fp.clone(),
            observed.clone(),
        ));

        let mut tls_config = ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        tls_config.dangerous().set_certificate_verifier(verifier);
        let connector = TlsConnector::from(Arc::new(tls_config));

        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .with_context(|| format!("failed to connect to {normalized_addr}"))?;
        let _ = tcp.set_nodelay(true);
        let domain = ServerName::try_from("kvm-server")
            .map_err(|_| anyhow::anyhow!("invalid server name"))?
            .to_owned();

        let mut stream = match connector.connect(domain, tcp).await {
            Ok(s) => s,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("fingerprint mismatch") {
                    return Err(anyhow::Error::new(FatalConnectError(msg)));
                }
                return Err(anyhow::Error::from(e))
                    .with_context(|| format!("TLS handshake failed with {normalized_addr}"));
            }
        };

        let (file_transfer_port, server_name) =
            perform_handshake(&mut stream, config, opts).await?;

        let observed_fp = observed.lock().unwrap().clone().unwrap_or_default();
        if first_use {
            info!("Pinned server certificate FP: {}", observed_fp);
        }
        config.add_known_server(
            server_name.clone(),
            normalized_addr.clone(),
            observed_fp.clone(),
        );
        // The server presents the same certificate on its file-transfer
        // port, but `known_servers` is keyed by the exact address that was
        // connected to. Record the fingerprint under the file-port address
        // too so `file_transfer::send_file` (which looks up
        // `host:file_transfer_port`) finds a pin instead of silently
        // connecting unpinned. See C7.
        let file_addr = crate::addr::format_host_port(&host, file_transfer_port);
        if file_addr != normalized_addr {
            config.add_known_server_alias(server_name.clone(), file_addr, observed_fp.clone());
        }

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        Ok(Session {
            stream,
            host,
            server_name,
            fingerprint: observed_fp,
            file_transfer_port,
            cmd_tx,
            cmd_rx,
        })
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn file_transfer_port(&self) -> u16 {
        self.file_transfer_port
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn controller(&self) -> SessionController {
        SessionController {
            tx: self.cmd_tx.clone(),
        }
    }

    /// Drive the session until `cancel` fires, the server disconnects, or a
    /// keepalive timeout elapses. Consumes `self`: a session is single-use.
    pub async fn run(
        mut self,
        cancel: CancellationToken,
        events: mpsc::Sender<SessionEvent>,
    ) -> Result<()> {
        let _ = events
            .send(SessionEvent::Connected {
                server_name: self.server_name.clone(),
                fingerprint: self.fingerprint.clone(),
            })
            .await;

        let injector = match InputInjector::new() {
            Ok(i) => i,
            Err(e) => {
                let _ = events
                    .send(SessionEvent::Error(format!(
                        "input injection unavailable: {e}"
                    )))
                    .await;
                return Err(e);
            }
        };

        let (mut read_half, mut write_half) = tokio::io::split(self.stream);

        let (clip_out_tx, mut clip_out_rx) = mpsc::unbounded_channel::<String>();
        let (clip_in_tx, clip_in_rx) = std::sync::mpsc::channel::<String>();
        // `spawn` reports whether the clipboard actually opened. When it
        // didn't (e.g. a headless/SSH session with no X11/Wayland display),
        // `clip_out_tx` is dropped on the watcher thread with no message
        // ever sent, which makes `clip_out_rx.recv()` resolve to `None` on
        // *every* poll instead of ever pending -- so the `select!` arm below
        // is gated on this flag to avoid busy-spinning the whole session
        // loop at 100% CPU for the rest of the connection's life.
        let clipboard_enabled = crate::clipboard::spawn(clip_out_tx, clip_in_rx);

        let mut ping_interval = tokio::time::interval(Duration::from_secs(10));
        let mut last_received = tokio::time::Instant::now();
        const DEAD_AFTER: Duration = Duration::from_secs(20);

        let result: Result<()> = 'outer: loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = send_envelope(&mut write_half, &Envelope::Goodbye).await;
                    break Ok(());
                }
                _ = ping_interval.tick() => {
                    if last_received.elapsed() > DEAD_AFTER {
                        break Err(anyhow::anyhow!("server unresponsive (no data for {:?})", DEAD_AFTER));
                    }
                    if send_envelope(&mut write_half, &Envelope::Ping(now_millis())).await.is_err() {
                        break Err(anyhow::anyhow!("connection lost while sending keepalive ping"));
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let envelope = match cmd {
                        Some(Command::ToggleForward(on)) => Envelope::ToggleForward(on),
                        Some(Command::SendClipboard(text)) => Envelope::ClipboardSet { text },
                        None => continue 'outer,
                    };
                    if send_envelope(&mut write_half, &envelope).await.is_err() {
                        break Err(anyhow::anyhow!("connection lost while sending a command"));
                    }
                }
                clip = clip_out_rx.recv(), if clipboard_enabled => {
                    if let Some(text) = clip {
                        let _ = send_envelope(&mut write_half, &Envelope::ClipboardSet { text }).await;
                    }
                }
                envelope = read_envelope(&mut read_half) => {
                    match envelope {
                        Ok(env) => {
                            last_received = tokio::time::Instant::now();
                            match env {
                                Envelope::Input(Event::ReleaseAll) => injector.release_all(),
                                Envelope::Input(ev) => {
                                    if let Err(e) = injector.inject(ev) {
                                        warn!("failed to inject event: {}", e);
                                    }
                                }
                                Envelope::ClipboardSet { text } => {
                                    let bytes = text.len();
                                    let _ = clip_in_tx.send(text);
                                    let _ = events.send(SessionEvent::ClipboardReceived { bytes }).await;
                                }
                                Envelope::ToggleForward(on) => {
                                    if !on {
                                        injector.release_all();
                                    }
                                    let _ = events.send(SessionEvent::ForwardingChanged(on)).await;
                                }
                                Envelope::Ping(ts) => {
                                    if send_envelope(&mut write_half, &Envelope::Pong(ts)).await.is_err() {
                                        break Err(anyhow::anyhow!("connection lost while replying to ping"));
                                    }
                                }
                                Envelope::Pong(ts) => {
                                    let _ = events.send(SessionEvent::Latency { ms: now_millis().saturating_sub(ts) }).await;
                                }
                                Envelope::Goodbye => break Ok(()),
                                Envelope::Error(msg) => {
                                    let _ = events.send(SessionEvent::Error(msg)).await;
                                }
                                other => warn!("unexpected message from server: {:?}", other),
                            }
                        }
                        Err(e) => break Err(anyhow::anyhow!("connection closed: {}", e)),
                    }
                }
            }
        };

        injector.release_all();
        match &result {
            Ok(()) => {
                let _ = events
                    .send(SessionEvent::Disconnected {
                        reason: "closed".to_string(),
                    })
                    .await;
            }
            Err(e) => {
                let _ = events
                    .send(SessionEvent::Disconnected {
                        reason: e.to_string(),
                    })
                    .await;
            }
        }
        result
    }
}

/// The post-TLS half of connecting: `Hello` / `HelloAck`, then
/// `PairRequest` / `PairAccept`. Generic over the stream so it can run over
/// a real (TLS) connection or, in tests, a plain `tokio::io::duplex` pair
/// against a fake server -- the pairing logic (in particular, that the
/// proof sent actually matches the configured PIN) doesn't depend on TLS at
/// all.
///
/// On success, returns `(file_transfer_port, server_name)` and leaves
/// `config` updated with a freshly generated `client_secret` if it didn't
/// have one yet (the caller still owns persisting `known_servers`, since
/// that also needs the certificate fingerprint this function never sees).
pub async fn perform_handshake<S>(
    stream: &mut S,
    config: &mut Config,
    opts: &ConnectOptions,
) -> Result<(u16, String)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "kvm-client".to_string());

    send_envelope(
        stream,
        &Envelope::Hello {
            name: hostname,
            version: PROTOCOL_VERSION.to_string(),
            caps: Caps::default(),
        },
    )
    .await?;

    let (file_transfer_port, server_name) = match read_envelope(stream).await? {
        Envelope::HelloAck {
            file_transfer_port,
            server_name,
        } => (file_transfer_port, server_name),
        Envelope::Error(msg) => {
            if msg.to_lowercase().contains("version") {
                return Err(anyhow::Error::new(FatalConnectError(msg)));
            }
            anyhow::bail!("server error: {}", msg);
        }
        other => anyhow::bail!("unexpected response: {:?}", other),
    };

    config.ensure_client_secret();
    let fingerprint = config.client_fingerprint();
    let pin = opts
        .pin
        .clone()
        .or_else(|| std::env::var("KVM_PAIRING_PIN").ok())
        .unwrap_or_default();
    let nonce: [u8; 16] = rand::random();
    let proof = pairing_proof(&pin, &nonce, &fingerprint);

    send_envelope(
        stream,
        &Envelope::PairRequest {
            nonce,
            fingerprint: fingerprint.clone(),
            proof,
        },
    )
    .await?;

    match read_envelope(stream).await? {
        Envelope::PairAccept => {}
        Envelope::PairReject { reason } => {
            let mut msg = format!("pairing rejected: {reason}");
            if reason.to_lowercase().contains("pin") {
                msg.push_str(" (pass --pin <code>, or set KVM_PAIRING_PIN)");
            }
            return Err(anyhow::Error::new(FatalConnectError(msg)));
        }
        other => anyhow::bail!("unexpected pairing response: {:?}", other),
    }

    Ok((file_transfer_port, server_name))
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Connect and run, retrying with exponential backoff (1s -> 30s, uncapped
/// attempts) until `cancel` fires. A connect-time error that is fatal (bad
/// PIN, version mismatch, certificate fingerprint mismatch) is *not*
/// retried: it is reported and returned immediately.
pub async fn run_with_reconnect(
    addr: String,
    config_path: Option<PathBuf>,
    opts: ConnectOptions,
    cancel: CancellationToken,
    events: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    let path = match config_path {
        Some(p) => p,
        None => Config::default_path()?,
    };

    let mut backoff = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(30);

    while !cancel.is_cancelled() {
        let mut config = Config::load_or_default(&path);
        match Session::connect(&addr, &mut config, &opts).await {
            Ok(session) => {
                backoff = Duration::from_secs(1);
                if let Err(e) = config.save(&path) {
                    warn!("failed to save client config at {}: {}", path.display(), e);
                }
                if let Err(e) = session.run(cancel.clone(), events.clone()).await {
                    warn!("session ended: {}", e);
                }
            }
            Err(e) => {
                if let Some(fatal) = e.downcast_ref::<FatalConnectError>() {
                    let _ = events.send(SessionEvent::Error(fatal.0.clone())).await;
                    return Err(e);
                }
                warn!("connection attempt failed: {}", e);
                let _ = events.send(SessionEvent::Error(e.to_string())).await;
            }
        }

        if cancel.is_cancelled() {
            break;
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
    Ok(())
}
