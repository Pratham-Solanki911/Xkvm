//! One dedicated OS thread owns the single `arboard::Clipboard` handle for a
//! session's whole lifetime: polling it for local changes to forward to the
//! server, and applying remote changes the server sends down. Keeping both
//! directions on the same thread means there is exactly one clipboard
//! change we just made ourselves to remember, so a value we set never gets
//! echoed straight back.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

/// Start the clipboard watcher thread.
///
/// * `outgoing`: local clipboard changes worth sending to the server land here.
/// * `incoming`: text received from the server to apply to the local clipboard.
///
/// Returns `true` if the clipboard was actually opened and the watcher loop
/// is running, `false` if `arboard::Clipboard::new()` failed (no clipboard
/// access on this platform/session) -- in which case this logs once and the
/// thread exits quietly without ever touching `outgoing`/`incoming` again.
///
/// Callers MUST NOT keep polling `outgoing` unconditionally when this
/// returns `false`: with no sender ever produced, the receiving end of an
/// `UnboundedReceiver` reports "closed" (`Ready(None)`) on every poll rather
/// than blocking, so an unconditional `recv().await` on it inside a
/// `select!` loop would busy-spin forever instead of ever going idle. Gate
/// that `select!` arm on this return value instead (e.g. `, if
/// clipboard_enabled`).
pub fn spawn(outgoing: UnboundedSender<String>, incoming: Receiver<String>) -> bool {
    spawn_with(arboard::Clipboard::new, outgoing, incoming)
}

/// Same as [`spawn`], but takes the clipboard-opening call as a factory so
/// tests can force the "unavailable" path deterministically instead of
/// depending on whatever clipboard access happens to exist in the test
/// environment.
fn spawn_with<F>(open: F, outgoing: UnboundedSender<String>, incoming: Receiver<String>) -> bool
where
    F: FnOnce() -> Result<arboard::Clipboard, arboard::Error> + Send + 'static,
{
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();
    std::thread::spawn(move || {
        let mut clipboard = match open() {
            Ok(c) => c,
            Err(e) => {
                warn!("clipboard unavailable, clipboard sync disabled: {}", e);
                let _ = ready_tx.send(false);
                return;
            }
        };
        let _ = ready_tx.send(true);

        let mut last_seen: Option<String> = None;

        loop {
            loop {
                match incoming.try_recv() {
                    Ok(text) => {
                        if clipboard.set_text(text.clone()).is_ok() {
                            last_seen = Some(text);
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            if let Ok(text) = clipboard.get_text() {
                if !text.is_empty()
                    && text.len() <= MAX_CLIPBOARD_BYTES
                    && last_seen.as_deref() != Some(text.as_str())
                {
                    last_seen = Some(text.clone());
                    if outgoing.send(text).is_err() {
                        return;
                    }
                }
            }

            std::thread::sleep(POLL_INTERVAL);
        }
    });

    // Block briefly until the thread reports whether it actually opened the
    // clipboard. If the send-side is dropped without a message (thread
    // panicked before sending, which shouldn't happen but must not hang the
    // caller), treat that the same as "disabled".
    ready_rx.recv().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// When the clipboard fails to open, `spawn` must report that promptly
    /// (not hang) and the `outgoing` sender must be dropped without ever
    /// sending anything -- this is exactly the condition that used to make
    /// `session.rs`'s `select! { clip = clip_out_rx.recv() => .. }` arm
    /// resolve to `None` on every single poll and busy-spin the whole
    /// session loop at 100% CPU instead of ever going idle.
    #[test]
    fn disabled_clipboard_reports_false_and_drops_outgoing_without_sending() {
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (_in_tx, in_rx) = std::sync::mpsc::channel::<String>();

        let enabled = spawn_with(|| Err(arboard::Error::ClipboardNotSupported), out_tx, in_rx);

        assert!(
            !enabled,
            "spawn_with must report false when the clipboard fails to open"
        );

        // The watcher thread returned immediately after reporting failure,
        // dropping its `outgoing` sender without ever sending a message --
        // so the receiver is closed-and-empty, i.e. `try_recv` reports
        // `Disconnected`, not `Empty`. (This is the exact channel state a
        // caller must detect via the `enabled` flag rather than by polling
        // `recv()` in a `select!`, since a closed-and-empty unbounded
        // channel resolves `recv()` to `Ready(None)` on every poll instead
        // of pending.)
        for _ in 0..20 {
            match out_rx.try_recv() {
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(text) => panic!("unexpected clipboard text sent while disabled: {text:?}"),
            }
        }
        panic!("outgoing channel never closed after the clipboard watcher gave up");
    }

    /// `spawn` (the real entry point, not `spawn_with`) must resolve
    /// promptly either way -- whether or not this machine actually has
    /// clipboard access (headless CI often doesn't). Whichever answer comes
    /// back, `spawn` itself must not hang, and the channels must be left in
    /// a consistent state for it.
    #[test]
    fn spawn_reports_readiness_without_hanging() {
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (_in_tx, in_rx) = std::sync::mpsc::channel::<String>();

        let started = std::time::Instant::now();
        let enabled = spawn(out_tx, in_rx);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "spawn() must report clipboard availability promptly, not hang"
        );

        if !enabled {
            // Same contract as the dedicated disabled-path test above: no
            // message ever arrives on `outgoing`.
            for _ in 0..20 {
                match out_rx.try_recv() {
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(_) => return, // a real (if surprising) clipboard change
                }
            }
        }
    }
}
