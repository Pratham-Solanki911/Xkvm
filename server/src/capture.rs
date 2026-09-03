//! Global input capture.
//!
//! A single [`InputCapture`] runs for the life of the server (see
//! `server::run_server`). Its callback runs on rdev's dedicated OS thread and
//! sees *every* input event regardless of whether forwarding is currently
//! enabled - this is what lets the configured hotkey toggle forwarding even
//! while it is off, and lets mouse-position tracking stay accurate across
//! toggles.
//!
//! On Windows and macOS, `rdev::grab` is used (via the `unstable_grab`
//! feature) so local key/button/wheel input can be swallowed while
//! forwarding is on. On Linux there is no non-root/evdev-exclusive grab
//! support in this configuration, so `rdev::listen` is used instead: input is
//! mirrored to the paired client but never blocked locally.
//!
//! Because the callback runs on rdev's own thread and (for `grab`) must be
//! `Fn` (not `FnMut`), all shared state is behind atomics or
//! `std::sync::Mutex`, and events leave the thread only via channel senders.

use crate::hotkey::HotkeyChord;
use kvm_common::{modifiers, Event, MouseButton};
use rdev::{Button, EventType, Key};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{error, info};

/// Minimum time between hotkey activations.
const HOTKEY_DEBOUNCE: Duration = Duration::from_millis(250);
/// Distance (in pixels) from a screen edge that triggers the cursor warp.
const EDGE_THRESHOLD: f64 = 2.0;

/// What the capture callback hands back to `run_server`'s forwarder task.
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    /// An input event to forward to paired clients (only emitted while
    /// forwarding is enabled).
    Input(Event),
    /// The hotkey fired and flipped forwarding to this new state.
    ForwardingToggled(bool),
}

struct CaptureState {
    enabled: Arc<AtomicBool>,
    was_enabled: AtomicBool,
    last_x: Mutex<f64>,
    last_y: Mutex<f64>,
    pressed_keys: Mutex<HashSet<Key>>,
    /// Set right after we programmatically warp the cursor back to center;
    /// the next MouseMove is that warp completing, not real user motion.
    warping: AtomicBool,
    /// Set when forwarding just turned on; the next MouseMove only seeds
    /// `last_x`/`last_y` instead of producing a (likely huge) delta.
    seed_next: AtomicBool,
    last_hotkey_fire: Mutex<Option<Instant>>,
    hotkey: Option<HotkeyChord>,
    /// Keys whose most recent *press* was consumed as (part of) the hotkey
    /// chord and therefore swallowed. Only a key in this set has its
    /// *release* swallowed too - otherwise an ordinary press of the hotkey's
    /// base key (no modifiers held) would have its release eaten regardless,
    /// leaving the key stuck down both locally (under `grab`) and on any
    /// remote client that had received a normal, unswallowed press.
    hotkey_swallowed_keys: Mutex<HashSet<Key>>,
}

pub struct InputCapture {
    enabled: Arc<AtomicBool>,
}

impl InputCapture {
    /// `enabled` is the single source of truth for whether forwarding is on;
    /// it is shared with (and can be toggled by) the rest of the server.
    pub fn new(enabled: Arc<AtomicBool>) -> Self {
        Self { enabled }
    }

    pub fn set_enabled(&self, value: bool) {
        self.enabled.store(value, Ordering::SeqCst);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Starts capturing on a dedicated OS thread. `tx` receives both
    /// forwarded input events and hotkey toggle notifications; `hotkey` is
    /// the parsed toggle chord (`None` disables hotkey handling entirely,
    /// e.g. because the configured string failed to parse).
    pub fn start(&self, tx: mpsc::UnboundedSender<CaptureEvent>, hotkey: Option<HotkeyChord>) {
        let state = Arc::new(CaptureState {
            enabled: self.enabled.clone(),
            was_enabled: AtomicBool::new(self.enabled.load(Ordering::SeqCst)),
            last_x: Mutex::new(0.0),
            last_y: Mutex::new(0.0),
            pressed_keys: Mutex::new(HashSet::new()),
            warping: AtomicBool::new(false),
            seed_next: AtomicBool::new(true),
            last_hotkey_fire: Mutex::new(None),
            hotkey,
            hotkey_swallowed_keys: Mutex::new(HashSet::new()),
        });

        #[cfg(any(windows, target_os = "macos"))]
        {
            let state = state.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = rdev::grab(move |event| handle_grab_event(&state, &tx, event)) {
                    error!("Failed to grab input events: {:?}", e);
                }
            });
        }

        #[cfg(not(any(windows, target_os = "macos")))]
        {
            info!(
                "Local input is mirrored, not blocked, on this platform \
                 (no exclusive-grab support here); the remote side sees every \
                 keystroke and click while forwarding is on, but so does this \
                 machine."
            );
            std::thread::spawn(move || {
                if let Err(e) = rdev::listen(move |event| handle_listen_event(&state, &tx, event)) {
                    error!("Failed to listen to input events: {:?}", e);
                }
            });
        }
    }
}

#[derive(Default)]
struct ProcessOutcome {
    /// Whether the local OS should NOT see this event (only meaningful under
    /// `grab`; `listen` can't block anything regardless).
    swallow: bool,
    forward: Option<Event>,
    toggled: Option<bool>,
}

#[cfg(any(windows, target_os = "macos"))]
fn handle_grab_event(
    state: &CaptureState,
    tx: &mpsc::UnboundedSender<CaptureEvent>,
    event: rdev::Event,
) -> Option<rdev::Event> {
    let outcome = process_event(state, &event);
    dispatch(tx, &outcome);
    if outcome.swallow {
        None
    } else {
        Some(event)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn handle_listen_event(
    state: &CaptureState,
    tx: &mpsc::UnboundedSender<CaptureEvent>,
    event: rdev::Event,
) {
    let outcome = process_event(state, &event);
    dispatch(tx, &outcome);
}

fn dispatch(tx: &mpsc::UnboundedSender<CaptureEvent>, outcome: &ProcessOutcome) {
    if let Some(toggled) = outcome.toggled {
        info!(
            "Input forwarding {} via hotkey",
            if toggled { "ENABLED" } else { "DISABLED" }
        );
        let _ = tx.send(CaptureEvent::ForwardingToggled(toggled));
    }
    if let Some(ref ev) = outcome.forward {
        let _ = tx.send(CaptureEvent::Input(ev.clone()));
    }
}

fn process_event(state: &CaptureState, event: &rdev::Event) -> ProcessOutcome {
    let now_enabled = state.enabled.load(Ordering::SeqCst);
    let was_enabled = state.was_enabled.swap(now_enabled, Ordering::SeqCst);
    if now_enabled && !was_enabled {
        // Forwarding just turned on (hotkey, client request, or auto_forward);
        // don't let the next MouseMove emit a delta against a stale position.
        state.seed_next.store(true, Ordering::SeqCst);
    }

    let mut outcome = ProcessOutcome::default();

    match event.event_type {
        EventType::KeyPress(key) => {
            let (is_repeat, mods) = {
                let mut pressed = state.pressed_keys.lock().unwrap();
                let is_repeat = pressed.contains(&key);
                pressed.insert(key);
                (is_repeat, compute_modifiers(&pressed))
            };

            if let Some(hk) = state.hotkey {
                if key == hk.key && chord_satisfied(&hk, mods) {
                    outcome.swallow = true;
                    state.hotkey_swallowed_keys.lock().unwrap().insert(key);
                    if !is_repeat {
                        outcome.toggled = try_fire_hotkey(state, now_enabled);
                    }
                    return outcome;
                }
            }

            if now_enabled {
                outcome.swallow = true;
                outcome.forward = Some(Event::KeyPress {
                    code: key_to_code(key),
                    modifiers: mods,
                });
            }
        }
        EventType::KeyRelease(key) => {
            let mods = {
                let mut pressed = state.pressed_keys.lock().unwrap();
                pressed.remove(&key);
                compute_modifiers(&pressed)
            };

            // Only swallow this release if the *matching press* was actually
            // consumed as the hotkey chord - not merely because this key
            // happens to be the hotkey's base key. Otherwise an ordinary
            // press+release of e.g. plain 'F' (no Ctrl/Alt held) would have
            // its press forwarded/typed normally but its release eaten,
            // leaving the key stuck down.
            if state.hotkey_swallowed_keys.lock().unwrap().remove(&key) {
                outcome.swallow = true;
                return outcome;
            }

            if now_enabled {
                outcome.swallow = true;
                outcome.forward = Some(Event::KeyRelease {
                    code: key_to_code(key),
                    modifiers: mods,
                });
            }
        }
        EventType::ButtonPress(button) => {
            if now_enabled {
                outcome.swallow = true;
                outcome.forward = Some(Event::MouseButtonPress(convert_mouse_button(button)));
            }
        }
        EventType::ButtonRelease(button) => {
            if now_enabled {
                outcome.swallow = true;
                outcome.forward = Some(Event::MouseButtonRelease(convert_mouse_button(button)));
            }
        }
        EventType::Wheel { delta_x, delta_y } => {
            if now_enabled {
                outcome.swallow = true;
                outcome.forward = Some(Event::Wheel {
                    delta_x: delta_x as i32,
                    delta_y: delta_y as i32,
                });
            }
        }
        EventType::MouseMove { x, y } => {
            // Never swallow MouseMove: the local cursor must keep moving
            // even while forwarding is on.
            outcome.forward = process_mouse_move(state, x, y, now_enabled);
        }
    }

    outcome
}

fn try_fire_hotkey(state: &CaptureState, now_enabled: bool) -> Option<bool> {
    let mut last_fire = state.last_hotkey_fire.lock().unwrap();
    let now = Instant::now();
    if let Some(prev) = *last_fire {
        if now.duration_since(prev) < HOTKEY_DEBOUNCE {
            return None;
        }
    }
    *last_fire = Some(now);
    drop(last_fire);

    let new_state = !now_enabled;
    state.enabled.store(new_state, Ordering::SeqCst);
    state.was_enabled.store(new_state, Ordering::SeqCst);
    if new_state {
        state.seed_next.store(true, Ordering::SeqCst);
    }
    Some(new_state)
}

fn process_mouse_move(state: &CaptureState, x: f64, y: f64, now_enabled: bool) -> Option<Event> {
    let mut last_x = state.last_x.lock().unwrap();
    let mut last_y = state.last_y.lock().unwrap();

    if state.warping.swap(false, Ordering::SeqCst) {
        *last_x = x;
        *last_y = y;
        return None;
    }

    if state.seed_next.swap(false, Ordering::SeqCst) {
        *last_x = x;
        *last_y = y;
        return None;
    }

    let dx = (x - *last_x) as i32;
    let dy = (y - *last_y) as i32;
    *last_x = x;
    *last_y = y;

    if now_enabled {
        if let Ok((w, h)) = rdev::display_size() {
            if w > 0 && h > 0 {
                let near_edge = x <= EDGE_THRESHOLD
                    || y <= EDGE_THRESHOLD
                    || x >= (w as f64 - 1.0 - EDGE_THRESHOLD)
                    || y >= (h as f64 - 1.0 - EDGE_THRESHOLD);
                if near_edge {
                    let center_x = w as f64 / 2.0;
                    let center_y = h as f64 / 2.0;
                    if rdev::simulate(&EventType::MouseMove {
                        x: center_x,
                        y: center_y,
                    })
                    .is_ok()
                    {
                        state.warping.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }

    // Only ever forward motion while forwarding is actually enabled - the
    // edge-warp above already required `now_enabled`, but the delta itself
    // must be gated too, or mouse position is streamed to every paired
    // client regardless of the forwarding toggle.
    if now_enabled && (dx != 0 || dy != 0) {
        Some(Event::MouseDelta { dx, dy })
    } else {
        None
    }
}

fn compute_modifiers(pressed: &HashSet<Key>) -> u8 {
    let mut mods = 0u8;
    if pressed.contains(&Key::ControlLeft) || pressed.contains(&Key::ControlRight) {
        mods |= modifiers::CTRL;
    }
    if pressed.contains(&Key::Alt) || pressed.contains(&Key::AltGr) {
        mods |= modifiers::ALT;
    }
    if pressed.contains(&Key::ShiftLeft) || pressed.contains(&Key::ShiftRight) {
        mods |= modifiers::SHIFT;
    }
    if pressed.contains(&Key::MetaLeft) || pressed.contains(&Key::MetaRight) {
        mods |= modifiers::META;
    }
    mods
}

fn chord_satisfied(hk: &HotkeyChord, mods: u8) -> bool {
    let mut required = 0u8;
    if hk.ctrl {
        required |= modifiers::CTRL;
    }
    if hk.alt {
        required |= modifiers::ALT;
    }
    if hk.shift {
        required |= modifiers::SHIFT;
    }
    if hk.meta {
        required |= modifiers::META;
    }
    (mods & required) == required
}

fn convert_mouse_button(button: Button) -> MouseButton {
    match button {
        Button::Left => MouseButton::Left,
        Button::Right => MouseButton::Right,
        Button::Middle => MouseButton::Middle,
        Button::Unknown(4) => MouseButton::Back,
        Button::Unknown(5) => MouseButton::Forward,
        _ => MouseButton::Left,
    }
}

/// Maps an `rdev::Key` to the canonical (Linux evdev-derived) code shared by
/// server and client. `pub` because the client's cross-crate test asserts
/// every entry round-trips through `kvm_client::inject::code_to_key`.
pub fn key_to_code(key: Key) -> u32 {
    match key {
        Key::Escape => 1,
        Key::Num1 => 2,
        Key::Num2 => 3,
        Key::Num3 => 4,
        Key::Num4 => 5,
        Key::Num5 => 6,
        Key::Num6 => 7,
        Key::Num7 => 8,
        Key::Num8 => 9,
        Key::Num9 => 10,
        Key::Num0 => 11,
        Key::Minus => 12,
        Key::Equal => 13,
        Key::Backspace => 14,
        Key::Tab => 15,
        Key::KeyQ => 16,
        Key::KeyW => 17,
        Key::KeyE => 18,
        Key::KeyR => 19,
        Key::KeyT => 20,
        Key::KeyY => 21,
        Key::KeyU => 22,
        Key::KeyI => 23,
        Key::KeyO => 24,
        Key::KeyP => 25,
        Key::LeftBracket => 26,
        Key::RightBracket => 27,
        Key::Return => 28,
        Key::ControlLeft => 29,
        Key::KeyA => 30,
        Key::KeyS => 31,
        Key::KeyD => 32,
        Key::KeyF => 33,
        Key::KeyG => 34,
        Key::KeyH => 35,
        Key::KeyJ => 36,
        Key::KeyK => 37,
        Key::KeyL => 38,
        Key::SemiColon => 39,
        Key::Quote => 40,
        Key::BackQuote => 41,
        Key::ShiftLeft => 42,
        Key::BackSlash => 43,
        Key::KeyZ => 44,
        Key::KeyX => 45,
        Key::KeyC => 46,
        Key::KeyV => 47,
        Key::KeyB => 48,
        Key::KeyN => 49,
        Key::KeyM => 50,
        Key::Comma => 51,
        Key::Dot => 52,
        Key::Slash => 53,
        Key::ShiftRight => 54,
        Key::KpMultiply => 55,
        Key::Alt => 56,
        Key::Space => 57,
        Key::CapsLock => 58,
        Key::F1 => 59,
        Key::F2 => 60,
        Key::F3 => 61,
        Key::F4 => 62,
        Key::F5 => 63,
        Key::F6 => 64,
        Key::F7 => 65,
        Key::F8 => 66,
        Key::F9 => 67,
        Key::F10 => 68,
        Key::NumLock => 69,
        Key::ScrollLock => 70,
        Key::Kp7 => 71,
        Key::Kp8 => 72,
        Key::Kp9 => 73,
        Key::KpMinus => 74,
        Key::Kp4 => 75,
        Key::Kp5 => 76,
        Key::Kp6 => 77,
        Key::KpPlus => 78,
        Key::Kp1 => 79,
        Key::Kp2 => 80,
        Key::Kp3 => 81,
        Key::Kp0 => 82,
        Key::KpDelete => 83,
        Key::IntlBackslash => 86,
        Key::F11 => 87,
        Key::F12 => 88,
        Key::KpReturn => 96,
        Key::ControlRight => 97,
        Key::KpDivide => 98,
        Key::PrintScreen => 99,
        Key::AltGr => 100,
        Key::Home => 102,
        Key::UpArrow => 103,
        Key::PageUp => 104,
        Key::LeftArrow => 105,
        Key::RightArrow => 106,
        Key::End => 107,
        Key::DownArrow => 108,
        Key::PageDown => 109,
        Key::Insert => 110,
        Key::Delete => 111,
        Key::Pause => 119,
        Key::MetaLeft => 125,
        Key::MetaRight => 126,
        Key::Function => 464,
        Key::Unknown(n) => 0x1_0000 + n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet as StdHashSet;

    /// Every key in the canonical table (server `docs/PATCH_PLAN.md` table)
    /// maps to a unique, nonzero code.
    const CANONICAL_KEYS: &[Key] = &[
        Key::Escape,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
        Key::Num0,
        Key::Minus,
        Key::Equal,
        Key::Backspace,
        Key::Tab,
        Key::KeyQ,
        Key::KeyW,
        Key::KeyE,
        Key::KeyR,
        Key::KeyT,
        Key::KeyY,
        Key::KeyU,
        Key::KeyI,
        Key::KeyO,
        Key::KeyP,
        Key::LeftBracket,
        Key::RightBracket,
        Key::Return,
        Key::ControlLeft,
        Key::KeyA,
        Key::KeyS,
        Key::KeyD,
        Key::KeyF,
        Key::KeyG,
        Key::KeyH,
        Key::KeyJ,
        Key::KeyK,
        Key::KeyL,
        Key::SemiColon,
        Key::Quote,
        Key::BackQuote,
        Key::ShiftLeft,
        Key::BackSlash,
        Key::KeyZ,
        Key::KeyX,
        Key::KeyC,
        Key::KeyV,
        Key::KeyB,
        Key::KeyN,
        Key::KeyM,
        Key::Comma,
        Key::Dot,
        Key::Slash,
        Key::ShiftRight,
        Key::KpMultiply,
        Key::Alt,
        Key::Space,
        Key::CapsLock,
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::NumLock,
        Key::ScrollLock,
        Key::Kp7,
        Key::Kp8,
        Key::Kp9,
        Key::KpMinus,
        Key::Kp4,
        Key::Kp5,
        Key::Kp6,
        Key::KpPlus,
        Key::Kp1,
        Key::Kp2,
        Key::Kp3,
        Key::Kp0,
        Key::KpDelete,
        Key::IntlBackslash,
        Key::F11,
        Key::F12,
        Key::KpReturn,
        Key::ControlRight,
        Key::KpDivide,
        Key::PrintScreen,
        Key::AltGr,
        Key::Home,
        Key::UpArrow,
        Key::PageUp,
        Key::LeftArrow,
        Key::RightArrow,
        Key::End,
        Key::DownArrow,
        Key::PageDown,
        Key::Insert,
        Key::Delete,
        Key::Pause,
        Key::MetaLeft,
        Key::MetaRight,
        Key::Function,
    ];

    #[test]
    fn test_canonical_keys_map_to_unique_nonzero_codes() {
        let mut seen = StdHashSet::new();
        for &key in CANONICAL_KEYS {
            let code = key_to_code(key);
            assert_ne!(code, 0, "{:?} mapped to 0", key);
            assert!(
                seen.insert(code),
                "duplicate code {} for {:?} (already used)",
                code,
                key
            );
        }
    }

    #[test]
    fn test_unknown_key_code() {
        assert_eq!(key_to_code(Key::Unknown(5)), 0x10005);
        assert_eq!(key_to_code(Key::Unknown(0)), 0x10000);
    }

    #[test]
    fn test_specific_canonical_codes() {
        assert_eq!(key_to_code(Key::Escape), 1);
        assert_eq!(key_to_code(Key::KpMultiply), 55);
        assert_eq!(key_to_code(Key::NumLock), 69);
        assert_eq!(key_to_code(Key::ScrollLock), 70);
        assert_eq!(key_to_code(Key::KpDelete), 83);
        assert_eq!(key_to_code(Key::IntlBackslash), 86);
        assert_eq!(key_to_code(Key::KpReturn), 96);
        assert_eq!(key_to_code(Key::ControlRight), 97);
        assert_eq!(key_to_code(Key::KpDivide), 98);
        assert_eq!(key_to_code(Key::PrintScreen), 99);
        assert_eq!(key_to_code(Key::AltGr), 100);
        assert_eq!(key_to_code(Key::Pause), 119);
        assert_eq!(key_to_code(Key::Function), 464);
    }

    #[test]
    fn test_chord_satisfied_matches_subset_of_held_modifiers() {
        let hk = HotkeyChord {
            ctrl: true,
            alt: true,
            shift: false,
            meta: false,
            key: Key::KeyF,
        };
        assert!(chord_satisfied(&hk, modifiers::CTRL | modifiers::ALT));
        // Extra modifiers held don't prevent the match.
        assert!(chord_satisfied(
            &hk,
            modifiers::CTRL | modifiers::ALT | modifiers::SHIFT
        ));
        assert!(!chord_satisfied(&hk, modifiers::CTRL));
        assert!(!chord_satisfied(&hk, 0));
    }

    #[test]
    fn test_compute_modifiers_tracks_held_keys() {
        let mut pressed = StdHashSet::new();
        assert_eq!(compute_modifiers(&pressed), 0);
        pressed.insert(Key::ControlLeft);
        assert_eq!(compute_modifiers(&pressed), modifiers::CTRL);
        pressed.insert(Key::AltGr);
        assert_eq!(
            compute_modifiers(&pressed),
            modifiers::CTRL | modifiers::ALT
        );
    }

    fn make_test_state(enabled: bool, hotkey: Option<HotkeyChord>) -> CaptureState {
        CaptureState {
            enabled: Arc::new(AtomicBool::new(enabled)),
            was_enabled: AtomicBool::new(enabled),
            last_x: Mutex::new(100.0),
            last_y: Mutex::new(100.0),
            pressed_keys: Mutex::new(HashSet::new()),
            warping: AtomicBool::new(false),
            seed_next: AtomicBool::new(false),
            last_hotkey_fire: Mutex::new(None),
            hotkey,
            hotkey_swallowed_keys: Mutex::new(HashSet::new()),
        }
    }

    fn key_event(event_type: EventType) -> rdev::Event {
        rdev::Event {
            time: std::time::SystemTime::now(),
            name: None,
            event_type,
        }
    }

    // --- S2 (critical): MouseMove must never be forwarded while forwarding
    // is disabled, even though it is never *swallowed* (the local cursor has
    // to keep moving regardless of forwarding state). ---

    #[test]
    fn test_mouse_move_not_forwarded_when_forwarding_disabled() {
        let state = make_test_state(false, None);
        let outcome = process_event(
            &state,
            &key_event(EventType::MouseMove { x: 150.0, y: 130.0 }),
        );
        assert!(
            outcome.forward.is_none(),
            "mouse motion must not leak to paired clients while forwarding is off"
        );
        assert!(!outcome.swallow, "MouseMove is never locally swallowed");
    }

    #[test]
    fn test_mouse_move_forwarded_when_forwarding_enabled() {
        let state = make_test_state(true, None);
        let outcome = process_event(
            &state,
            &key_event(EventType::MouseMove { x: 150.0, y: 130.0 }),
        );
        match outcome.forward {
            Some(Event::MouseDelta { dx, dy }) => {
                assert_eq!(dx, 50);
                assert_eq!(dy, 30);
            }
            other => panic!(
                "expected a MouseDelta while forwarding is on, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_mouse_move_tracks_position_across_toggle_without_leaking_delta() {
        // Regression guard for the fix: position tracking (last_x/last_y)
        // must keep updating even while disabled, so re-enabling doesn't
        // produce a stale/huge delta - but no Input event should escape
        // while disabled.
        let state = make_test_state(false, None);
        let first = process_event(
            &state,
            &key_event(EventType::MouseMove { x: 150.0, y: 130.0 }),
        );
        assert!(first.forward.is_none());
        let second = process_event(
            &state,
            &key_event(EventType::MouseMove { x: 160.0, y: 135.0 }),
        );
        assert!(second.forward.is_none());
        assert_eq!(*state.last_x.lock().unwrap(), 160.0);
        assert_eq!(*state.last_y.lock().unwrap(), 135.0);
    }

    // --- High-severity: the hotkey's base key release must only be
    // swallowed when its matching press actually consumed the full chord. ---

    #[test]
    fn test_hotkey_key_release_swallowed_even_after_modifiers_released_early() {
        let hk = HotkeyChord {
            ctrl: true,
            alt: true,
            shift: false,
            meta: false,
            key: Key::KeyF,
        };
        let state = make_test_state(false, Some(hk));

        let _ = process_event(&state, &key_event(EventType::KeyPress(Key::ControlLeft)));
        let _ = process_event(&state, &key_event(EventType::KeyPress(Key::Alt)));
        let press_outcome = process_event(&state, &key_event(EventType::KeyPress(Key::KeyF)));
        assert!(press_outcome.swallow);
        assert_eq!(
            press_outcome.toggled,
            Some(true),
            "chord press should flip forwarding on"
        );

        // Modifiers let go before the hotkey's own key.
        let _ = process_event(&state, &key_event(EventType::KeyRelease(Key::ControlLeft)));
        let _ = process_event(&state, &key_event(EventType::KeyRelease(Key::Alt)));
        let release_outcome = process_event(&state, &key_event(EventType::KeyRelease(Key::KeyF)));
        assert!(
            release_outcome.swallow,
            "the release matching a chord-consumed press must still be swallowed, \
             or the key is left stuck down"
        );
    }

    #[test]
    fn test_plain_hotkey_base_key_release_not_swallowed() {
        // 'F' pressed and released with no Ctrl/Alt held at all - never
        // matches the chord, so both press and release go through the
        // ordinary now_enabled-gated forwarding path.
        let hk = HotkeyChord {
            ctrl: true,
            alt: true,
            shift: false,
            meta: false,
            key: Key::KeyF,
        };
        let state = make_test_state(true, Some(hk));

        let press_outcome = process_event(&state, &key_event(EventType::KeyPress(Key::KeyF)));
        assert!(press_outcome.toggled.is_none());
        assert!(matches!(
            press_outcome.forward,
            Some(Event::KeyPress { .. })
        ));

        let release_outcome = process_event(&state, &key_event(EventType::KeyRelease(Key::KeyF)));
        assert!(
            matches!(release_outcome.forward, Some(Event::KeyRelease { .. })),
            "an ordinary release must be forwarded, not silently eaten as a hotkey artifact"
        );
    }
}
