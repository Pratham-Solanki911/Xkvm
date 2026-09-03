#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

//! Tauri backend for the KVM-RS control panel. All the actual protocol,
//! pairing, TLS-pinning and file-transfer logic lives in `kvm-client`
//! (`kvm_client::*`); this file only wires that library up to Tauri
//! commands and events so the React frontend has something real to drive.

use kvm_client::{addr, discovery, file_transfer, Config, ConnectOptions, FatalConnectError};
use kvm_client::{Session, SessionController, SessionEvent};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Everything about the one control-channel connection the UI currently
/// holds, captured directly off the [`Session`] before it is handed to
/// `Session::run` (which consumes it) -- this is what lets
/// `send_file_to_server` reach the file port without re-deriving it from a
/// string address.
struct ActiveSession {
    cancel: CancellationToken,
    controller: SessionController,
    address: String,
    server_name: String,
    fingerprint: String,
    file_port: u16,
    host: String,
}

#[derive(Default)]
struct AppState {
    /// Bumped on every `connect_to_server` / `disconnect_from_server` call so
    /// a background reconnect task that has been superseded can tell it no
    /// longer owns `session` and stop touching shared state.
    generation: AtomicU64,
    session: Mutex<Option<ActiveSession>>,
    forwarding_active: AtomicBool,
    last_error: Mutex<Option<String>>,
    latency_ms: Mutex<Option<u64>>,
}

#[derive(Debug, Clone, Serialize)]
struct DiscoveredServerInfo {
    name: String,
    host: String,
    port: u16,
    address: String,
}

#[derive(Debug, Clone, Serialize)]
struct KnownServerInfo {
    name: String,
    address: String,
    fingerprint: String,
    last_connected: String,
}

#[derive(Debug, Clone, Serialize)]
struct KvmStatus {
    connected: bool,
    address: Option<String>,
    server_name: Option<String>,
    fingerprint: Option<String>,
    forwarding_active: bool,
    last_error: Option<String>,
    latency_ms: Option<u64>,
    version: String,
}

#[derive(Debug, Clone, Serialize)]
struct TransferProgress {
    file_name: String,
    sent: u64,
    total: u64,
    done: bool,
    error: Option<String>,
}

/// A JSON-friendly mirror of [`SessionEvent`], emitted to the frontend as
/// `kvm://status`. `SessionEvent` itself isn't `Serialize` (it's an internal
/// client type), so each variant is translated by hand.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StatusEvent {
    Connected {
        server_name: String,
        fingerprint: String,
    },
    Disconnected {
        reason: String,
    },
    ForwardingChanged {
        active: bool,
    },
    ClipboardReceived {
        bytes: usize,
    },
    Latency {
        ms: u64,
    },
    Error {
        message: String,
    },
}

impl From<&SessionEvent> for StatusEvent {
    fn from(e: &SessionEvent) -> Self {
        match e {
            SessionEvent::Connected {
                server_name,
                fingerprint,
            } => StatusEvent::Connected {
                server_name: server_name.clone(),
                fingerprint: fingerprint.clone(),
            },
            SessionEvent::Disconnected { reason } => StatusEvent::Disconnected {
                reason: reason.clone(),
            },
            SessionEvent::ForwardingChanged(on) => StatusEvent::ForwardingChanged { active: *on },
            SessionEvent::ClipboardReceived { bytes } => {
                StatusEvent::ClipboardReceived { bytes: *bytes }
            }
            SessionEvent::Latency { ms } => StatusEvent::Latency { ms: *ms },
            SessionEvent::Error(msg) => StatusEvent::Error {
                message: msg.clone(),
            },
        }
    }
}

#[tauri::command]
async fn discover_servers() -> Result<Vec<DiscoveredServerInfo>, String> {
    let servers = discovery::discover_servers(Duration::from_secs(3))
        .await
        .map_err(|e| e.to_string())?;
    Ok(servers
        .into_iter()
        .map(|s| DiscoveredServerInfo {
            name: s.name,
            host: s.host,
            port: s.port,
            address: s.address,
        })
        .collect())
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> Result<KvmStatus, String> {
    let guard = state.session.lock().unwrap();
    let (connected, address, server_name, fingerprint) = match &*guard {
        Some(s) => (
            true,
            Some(s.address.clone()),
            Some(s.server_name.clone()),
            Some(s.fingerprint.clone()),
        ),
        None => (false, None, None, None),
    };
    drop(guard);

    Ok(KvmStatus {
        connected,
        address,
        server_name,
        fingerprint,
        forwarding_active: state.forwarding_active.load(Ordering::SeqCst),
        last_error: state.last_error.lock().unwrap().clone(),
        latency_ms: *state.latency_ms.lock().unwrap(),
        version: kvm_common::PROTOCOL_VERSION.to_string(),
    })
}

#[tauri::command]
fn toggle_forwarding(state: State<'_, AppState>) -> Result<bool, String> {
    let guard = state.session.lock().unwrap();
    let active = guard
        .as_ref()
        .ok_or_else(|| "not connected to a server".to_string())?;
    let target = !state.forwarding_active.load(Ordering::SeqCst);
    active
        .controller
        .toggle_forward(target)
        .map_err(|e| e.to_string())?;
    Ok(target)
}

/// Cancel whatever session is currently active (if any) and bump the
/// generation counter so any background task still running for it becomes a
/// no-op the next time it checks in.
fn reset_session(state: &AppState) -> u64 {
    let my_gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(prev) = state.session.lock().unwrap().take() {
        prev.cancel.cancel();
    }
    state.forwarding_active.store(false, Ordering::SeqCst);
    my_gen
}

#[tauri::command]
fn disconnect_from_server(state: State<'_, AppState>) -> Result<(), String> {
    reset_session(&state);
    *state.last_error.lock().unwrap() = None;
    *state.latency_ms.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
async fn connect_to_server(
    address: String,
    pin: Option<String>,
    trust_new_cert: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let my_gen = reset_session(&state);

    let opts = ConnectOptions {
        pin,
        trust_new_cert,
        expected_fingerprint: None,
    };

    let path = Config::default_path().map_err(|e| e.to_string())?;
    let mut config = Config::load_or_default(&path);

    let session = Session::connect(&address, &mut config, &opts)
        .await
        .map_err(|e| e.to_string())?;

    if let Err(e) = config.save(&path) {
        warn!("failed to save client config at {}: {}", path.display(), e);
    }

    let (host, port) = addr::parse_host_port(&address, kvm_common::DEFAULT_CONTROL_PORT)
        .map_err(|e| e.to_string())?;
    let normalized = addr::format_host_port(&host, port);
    let fingerprint = config.known_fingerprint(&normalized).unwrap_or_default();

    let (events_tx, mut events_rx) = mpsc::channel::<SessionEvent>(32);
    let cancel = CancellationToken::new();

    if !install_active_session(&state, my_gen, &session, &cancel, &normalized, &fingerprint) {
        // A newer `connect_to_server` call (or a `disconnect_from_server`)
        // already bumped the generation while we were awaiting
        // `Session::connect` above -- this attempt has been superseded.
        // Tear it down here instead of spawning the event-forward and
        // session-driving tasks below: otherwise `session.run(..)` would
        // start the full protocol loop (including injecting real input from
        // whatever the now-invisible server sends) with no way for the user
        // to stop it, since it was never stored in `AppState`.
        cancel.cancel();
        drop(session);
        return Ok(());
    }

    // Forward every SessionEvent to the frontend and keep the coarse bits of
    // AppState (forwarding flag, last error, latency) in sync with it.
    {
        let app = app.clone();
        let my_gen_events = my_gen;
        tauri::async_runtime::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                let state = app.state::<AppState>();
                if state.generation.load(Ordering::SeqCst) != my_gen_events {
                    break;
                }
                match &event {
                    SessionEvent::ForwardingChanged(on) => {
                        state.forwarding_active.store(*on, Ordering::SeqCst);
                    }
                    SessionEvent::Latency { ms } => {
                        *state.latency_ms.lock().unwrap() = Some(*ms);
                    }
                    SessionEvent::Error(msg) => {
                        *state.last_error.lock().unwrap() = Some(msg.clone());
                    }
                    SessionEvent::Disconnected { reason } => {
                        *state.last_error.lock().unwrap() = Some(reason.clone());
                    }
                    _ => {}
                }
                let _ = app.emit_all("kvm://status", StatusEvent::from(&event));
            }
        });
    }

    // Drive the session, reconnecting with backoff on non-fatal failures,
    // until cancelled (by `disconnect_from_server` or a fresh
    // `connect_to_server` call) or a fatal error stops retries for good.
    {
        let app = app.clone();
        let normalized = normalized.clone();
        let cancel_loop = cancel.clone();
        tauri::async_runtime::spawn(async move {
            let mut session = session;
            let mut backoff = Duration::from_secs(1);
            const MAX_BACKOFF: Duration = Duration::from_secs(30);

            'outer: loop {
                if let Err(e) = session.run(cancel_loop.clone(), events_tx.clone()).await {
                    warn!("session ended: {}", e);
                }
                if cancel_loop.is_cancelled() {
                    break;
                }

                // Keep retrying (with backoff) until a fresh `Session` is
                // obtained, a fatal error stops retries for good, or we're
                // cancelled -- `session` must always be reassigned before
                // control returns to the top of the outer loop, since
                // `Session::run` above consumes it.
                session = 'reconnect: loop {
                    tokio::select! {
                        _ = cancel_loop.cancelled() => break 'outer,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);

                    let path = match Config::default_path() {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("cannot determine config path for reconnect: {}", e);
                            break 'outer;
                        }
                    };
                    let mut config = Config::load_or_default(&path);
                    match Session::connect(&normalized, &mut config, &opts).await {
                        Ok(new_session) => {
                            backoff = Duration::from_secs(1);
                            if let Err(e) = config.save(&path) {
                                warn!("failed to save client config: {}", e);
                            }
                            let fp = config.known_fingerprint(&normalized).unwrap_or_default();
                            let state = app.state::<AppState>();
                            let installed = install_active_session(
                                &state,
                                my_gen,
                                &new_session,
                                &cancel_loop,
                                &normalized,
                                &fp,
                            );
                            if !installed {
                                // Superseded between the reconnect attempt
                                // starting and finishing (a fresh
                                // `connect_to_server` or a
                                // `disconnect_from_server` landed in
                                // between). Drop this reconnected session
                                // instead of looping back to drive it with
                                // `session.run(..)` -- it was never stored
                                // in `AppState`, so it would otherwise run
                                // unsupervised with no way to stop it.
                                drop(new_session);
                                break 'outer;
                            }
                            break 'reconnect new_session;
                        }
                        Err(e) => {
                            let fatal = e.downcast_ref::<FatalConnectError>().map(|f| f.0.clone());
                            let state = app.state::<AppState>();
                            if state.generation.load(Ordering::SeqCst) != my_gen {
                                break 'outer;
                            }
                            let message = fatal.clone().unwrap_or_else(|| e.to_string());
                            *state.last_error.lock().unwrap() = Some(message.clone());
                            let _ = app.emit_all(
                                "kvm://status",
                                StatusEvent::Error {
                                    message: message.clone(),
                                },
                            );
                            if fatal.is_some() {
                                break 'outer;
                            }
                            warn!("reconnect attempt failed: {}", e);
                        }
                    }
                };
            }

            let state = app.state::<AppState>();
            if state.generation.load(Ordering::SeqCst) == my_gen {
                *state.session.lock().unwrap() = None;
                state.forwarding_active.store(false, Ordering::SeqCst);
            }
        });
    }

    Ok(())
}

/// Installs `session` into `AppState` as the active session, but only if
/// `my_gen` is still the current generation -- i.e. no newer
/// `connect_to_server` (or a `disconnect_from_server`) has superseded this
/// attempt since it started. Returns whether the install happened; the
/// caller MUST treat `false` as "this session has been superseded" and tear
/// it down (cancel its token, drop it) instead of driving it any further,
/// since a superseded session would otherwise keep running unsupervised
/// with no way for the user to reach it via `disconnect_from_server`.
fn install_active_session(
    state: &AppState,
    my_gen: u64,
    session: &Session,
    cancel: &CancellationToken,
    normalized_address: &str,
    fingerprint: &str,
) -> bool {
    if state.generation.load(Ordering::SeqCst) != my_gen {
        return false;
    }
    let mut guard = state.session.lock().unwrap();
    // Re-check under the lock: `reset_session` bumps the generation and then
    // takes/cancels whatever is in `state.session` under this same lock, so
    // holding it across both the generation check and the write closes the
    // race where a `reset_session` call lands between the load above and
    // this assignment.
    if state.generation.load(Ordering::SeqCst) != my_gen {
        return false;
    }
    *guard = Some(ActiveSession {
        cancel: cancel.clone(),
        controller: session.controller(),
        address: normalized_address.to_string(),
        server_name: session.server_name().to_string(),
        fingerprint: fingerprint.to_string(),
        file_port: session.file_transfer_port(),
        host: session.host().to_string(),
    });
    true
}

#[tauri::command]
async fn send_file_to_server(
    file_path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (host, port, expected_fingerprint) = {
        let guard = state.session.lock().unwrap();
        let active = guard
            .as_ref()
            .ok_or_else(|| "not connected to a server".to_string())?;
        (
            active.host.clone(),
            active.file_port,
            active.fingerprint.clone(),
        )
    };

    let path = std::path::PathBuf::from(&file_path);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| file_path.clone());

    let config_path = Config::default_path().map_err(|e| e.to_string())?;
    let config = Config::load_or_default(&config_path);
    let opts = ConnectOptions {
        pin: None,
        trust_new_cert: false,
        expected_fingerprint: Some(expected_fingerprint),
    };

    let (progress_tx, mut progress_rx) = mpsc::channel::<(u64, u64)>(32);
    {
        let app = app.clone();
        let file_name = file_name.clone();
        tauri::async_runtime::spawn(async move {
            while let Some((sent, total)) = progress_rx.recv().await {
                let _ = app.emit_all(
                    "kvm://transfer",
                    TransferProgress {
                        file_name: file_name.clone(),
                        sent,
                        total,
                        done: false,
                        error: None,
                    },
                );
            }
        });
    }

    let result =
        file_transfer::send_file(&host, port, &path, &config, &opts, Some(progress_tx)).await;

    let _ = app.emit_all(
        "kvm://transfer",
        TransferProgress {
            file_name: file_name.clone(),
            sent: 0,
            total: 0,
            done: true,
            error: result.as_ref().err().map(|e| e.to_string()),
        },
    );

    result.map_err(|e| e.to_string())
}

#[tauri::command]
fn list_known_servers() -> Result<Vec<KnownServerInfo>, String> {
    let path = Config::default_path().map_err(|e| e.to_string())?;
    let config = Config::load_or_default(&path);
    Ok(config
        .list_known_servers()
        .into_iter()
        .map(|s| KnownServerInfo {
            name: s.name,
            address: s.address,
            fingerprint: s.fingerprint,
            last_connected: s.last_connected.to_rfc3339(),
        })
        .collect())
}

#[tauri::command]
fn forget_server(address: String) -> Result<bool, String> {
    let path = Config::default_path().map_err(|e| e.to_string())?;
    let mut config = Config::load_or_default(&path);
    let removed = config.forget_server(&address);
    if removed {
        config.save(&path).map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            discover_servers,
            get_status,
            toggle_forwarding,
            connect_to_server,
            disconnect_from_server,
            send_file_to_server,
            list_known_servers,
            forget_server,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
