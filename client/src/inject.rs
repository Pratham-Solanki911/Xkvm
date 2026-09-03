//! Translates received [`Event`]s into real keyboard/mouse activity via
//! `enigo`, and tracks what is currently held down so it can all be released
//! at once (`ReleaseAll`, disconnect, or an injection error).

use anyhow::Result;
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use kvm_common::{Event, MouseButton};
use std::sync::Mutex;
use tracing::debug;

pub struct InputInjector {
    enigo: Mutex<Enigo>,
    /// Keys currently held, in press order, so `release_all` can release them
    /// in reverse (most-recently-pressed first).
    held_keys: Mutex<Vec<Key>>,
    held_buttons: Mutex<Vec<Button>>,
}

impl InputInjector {
    pub fn new() -> Result<Self> {
        let enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow::anyhow!("failed to initialize input injection: {e}"))?;
        Ok(Self {
            enigo: Mutex::new(enigo),
            held_keys: Mutex::new(Vec::new()),
            held_buttons: Mutex::new(Vec::new()),
        })
    }

    pub fn inject(&self, event: Event) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();

        match event {
            Event::MouseDelta { dx, dy } => {
                let _ = enigo.move_mouse(dx, dy, Coordinate::Rel);
            }
            Event::MouseButtonPress(button) => {
                let b = convert_mouse_button(button);
                let _ = enigo.button(b, Direction::Press);
                let mut held = self.held_buttons.lock().unwrap();
                if !held.contains(&b) {
                    held.push(b);
                }
            }
            Event::MouseButtonRelease(button) => {
                let b = convert_mouse_button(button);
                let _ = enigo.button(b, Direction::Release);
                self.held_buttons.lock().unwrap().retain(|h| *h != b);
            }
            Event::Wheel { delta_x, delta_y } => {
                // rdev reports delta_y in wheel "notches", positive meaning
                // the wheel rotated up/away from the user. enigo's
                // `scroll(length, Vertical)` is documented the other way
                // round (positive length scrolls *down*), so the vertical
                // axis is negated to keep "scroll up on the server" feeling
                // like "scroll up on the client". The horizontal axis's
                // sign already agrees (positive = right on both sides).
                if delta_y != 0 {
                    let _ = enigo.scroll(-delta_y, enigo::Axis::Vertical);
                }
                if delta_x != 0 {
                    let _ = enigo.scroll(delta_x, enigo::Axis::Horizontal);
                }
            }
            Event::KeyPress { code, modifiers: _ } => {
                if let Some(key) = code_to_key(code) {
                    let _ = enigo.key(key, Direction::Press);
                    let mut held = self.held_keys.lock().unwrap();
                    if !held.contains(&key) {
                        held.push(key);
                    }
                } else {
                    debug!("ignoring unknown key code: {}", code);
                }
            }
            Event::KeyRelease { code, modifiers: _ } => {
                if let Some(key) = code_to_key(code) {
                    let _ = enigo.key(key, Direction::Release);
                    self.held_keys.lock().unwrap().retain(|h| *h != key);
                } else {
                    debug!("ignoring unknown key code: {}", code);
                }
            }
            Event::ReleaseAll => {
                drop(enigo);
                self.release_all();
            }
        }

        Ok(())
    }

    /// Release every key and mouse button this injector believes is
    /// currently held, in reverse press order. Safe to call repeatedly
    /// (e.g. on every disconnect) even if nothing is held.
    pub fn release_all(&self) {
        let mut enigo = self.enigo.lock().unwrap();

        let keys: Vec<Key> = std::mem::take(&mut *self.held_keys.lock().unwrap());
        for key in keys.into_iter().rev() {
            let _ = enigo.key(key, Direction::Release);
        }

        let buttons: Vec<Button> = std::mem::take(&mut *self.held_buttons.lock().unwrap());
        for button in buttons.into_iter().rev() {
            let _ = enigo.button(button, Direction::Release);
        }
    }
}

fn convert_mouse_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
        MouseButton::Back => Button::Back,
        MouseButton::Forward => Button::Forward,
    }
}

// --- Numpad fallbacks -------------------------------------------------
//
// enigo's `Key::Numpad0..9`, `Add`, `Subtract`, `Multiply`, `Divide` and
// `Decimal` only exist on Windows (verified against
// enigo-0.2.1/src/keycodes.rs). Linux and macOS have no numpad-specific
// variants at all, so the closest available key is the matching digit /
// operator character, which is what these helpers fall back to there.

fn numpad_digit(d: u8) -> Key {
    #[cfg(target_os = "windows")]
    {
        match d {
            0 => Key::Numpad0,
            1 => Key::Numpad1,
            2 => Key::Numpad2,
            3 => Key::Numpad3,
            4 => Key::Numpad4,
            5 => Key::Numpad5,
            6 => Key::Numpad6,
            7 => Key::Numpad7,
            8 => Key::Numpad8,
            _ => Key::Numpad9,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        Key::Unicode((b'0' + d.min(9)) as char)
    }
}

fn numpad_multiply() -> Key {
    #[cfg(target_os = "windows")]
    {
        Key::Multiply
    }
    #[cfg(not(target_os = "windows"))]
    {
        Key::Unicode('*')
    }
}

fn numpad_minus() -> Key {
    #[cfg(target_os = "windows")]
    {
        Key::Subtract
    }
    #[cfg(not(target_os = "windows"))]
    {
        Key::Unicode('-')
    }
}

fn numpad_plus() -> Key {
    #[cfg(target_os = "windows")]
    {
        Key::Add
    }
    #[cfg(not(target_os = "windows"))]
    {
        Key::Unicode('+')
    }
}

fn numpad_divide() -> Key {
    #[cfg(target_os = "windows")]
    {
        Key::Divide
    }
    #[cfg(not(target_os = "windows"))]
    {
        Key::Unicode('/')
    }
}

fn numpad_delete() -> Key {
    // Numpad Del / "." key. `Decimal` (VK_DECIMAL) is the Windows
    // equivalent; elsewhere there is no numpad-specific key so fall back to
    // the plain Delete key, which is the closer behavioural match (numpad
    // Del with NumLock off deletes) than the '.' character.
    #[cfg(target_os = "windows")]
    {
        Key::Decimal
    }
    #[cfg(not(target_os = "windows"))]
    {
        Key::Delete
    }
}

fn numlock_key() -> Option<Key> {
    // `Numlock` exists on Windows and Linux/BSD (enigo cfg:
    // `any(windows, all(unix, not(macos)))`). macOS keyboards have no
    // NumLock key or clean equivalent, so there is nothing sensible to
    // press there.
    #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
    {
        Some(Key::Numlock)
    }
    #[cfg(not(any(target_os = "windows", all(unix, not(target_os = "macos")))))]
    {
        None
    }
}

fn scroll_lock_key() -> Key {
    #[cfg(target_os = "windows")]
    {
        Key::Scroll
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Key::ScrollLock
    }
    #[cfg(target_os = "macos")]
    {
        // Historical Mac keyboards mapped Scroll Lock to F14.
        Key::F14
    }
}

fn print_screen_key() -> Key {
    // `Print` covers both Windows and Linux/BSD in enigo (cfg:
    // `any(windows, all(unix, not(macos)))`).
    #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
    {
        Key::Print
    }
    #[cfg(target_os = "macos")]
    {
        // Historical Mac keyboards mapped Print Screen to F13.
        Key::F13
    }
}

fn pause_key() -> Key {
    #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
    {
        Key::Pause
    }
    #[cfg(target_os = "macos")]
    {
        // Historical Mac keyboards mapped Pause to F15.
        Key::F15
    }
}

fn insert_key() -> Key {
    // `Insert` covers Windows and Linux/BSD; macOS keyboards have no Insert
    // key, but the `Help` key (universal in enigo) occupies the same
    // physical position on Apple keyboards and doubles as Insert in
    // practice.
    #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
    {
        Key::Insert
    }
    #[cfg(target_os = "macos")]
    {
        Key::Help
    }
}

fn function_key() -> Key {
    // The `Fn` key only exists as an injectable key on macOS in enigo.
    // Windows/Linux keyboards have no independent `Fn` keycode at all (it's
    // usually intercepted by firmware), so map it to a high, rarely-bound
    // F-key there to avoid accidental side effects if it is ever received.
    #[cfg(target_os = "macos")]
    {
        Key::Function
    }
    #[cfg(not(target_os = "macos"))]
    {
        Key::F24
    }
}

/// Reverse of the server's `key_to_code`: maps a canonical evdev-style code
/// back to an `enigo::Key`. Codes `>= 0x1_0000` come from the server's
/// `Unknown(n)` fallback and are deliberately dropped (logged at debug).
pub fn code_to_key(code: u32) -> Option<Key> {
    if code >= 0x1_0000 {
        debug!(
            "ignoring server key code outside the canonical table: {}",
            code
        );
        return None;
    }

    Some(match code {
        1 => Key::Escape,
        2 => Key::Unicode('1'),
        3 => Key::Unicode('2'),
        4 => Key::Unicode('3'),
        5 => Key::Unicode('4'),
        6 => Key::Unicode('5'),
        7 => Key::Unicode('6'),
        8 => Key::Unicode('7'),
        9 => Key::Unicode('8'),
        10 => Key::Unicode('9'),
        11 => Key::Unicode('0'),
        12 => Key::Unicode('-'),
        13 => Key::Unicode('='),
        14 => Key::Backspace,
        15 => Key::Tab,
        16 => Key::Unicode('q'),
        17 => Key::Unicode('w'),
        18 => Key::Unicode('e'),
        19 => Key::Unicode('r'),
        20 => Key::Unicode('t'),
        21 => Key::Unicode('y'),
        22 => Key::Unicode('u'),
        23 => Key::Unicode('i'),
        24 => Key::Unicode('o'),
        25 => Key::Unicode('p'),
        26 => Key::Unicode('['),
        27 => Key::Unicode(']'),
        28 => Key::Return,
        29 => Key::LControl,
        30 => Key::Unicode('a'),
        31 => Key::Unicode('s'),
        32 => Key::Unicode('d'),
        33 => Key::Unicode('f'),
        34 => Key::Unicode('g'),
        35 => Key::Unicode('h'),
        36 => Key::Unicode('j'),
        37 => Key::Unicode('k'),
        38 => Key::Unicode('l'),
        39 => Key::Unicode(';'),
        40 => Key::Unicode('\''),
        41 => Key::Unicode('`'),
        42 => Key::LShift,
        43 => Key::Unicode('\\'),
        44 => Key::Unicode('z'),
        45 => Key::Unicode('x'),
        46 => Key::Unicode('c'),
        47 => Key::Unicode('v'),
        48 => Key::Unicode('b'),
        49 => Key::Unicode('n'),
        50 => Key::Unicode('m'),
        51 => Key::Unicode(','),
        52 => Key::Unicode('.'),
        53 => Key::Unicode('/'),
        54 => Key::RShift,
        55 => numpad_multiply(),
        56 => Key::Alt,
        57 => Key::Space,
        58 => Key::CapsLock,
        59 => Key::F1,
        60 => Key::F2,
        61 => Key::F3,
        62 => Key::F4,
        63 => Key::F5,
        64 => Key::F6,
        65 => Key::F7,
        66 => Key::F8,
        67 => Key::F9,
        68 => Key::F10,
        69 => return numlock_key(),
        70 => scroll_lock_key(),
        71 => numpad_digit(7),
        72 => numpad_digit(8),
        73 => numpad_digit(9),
        74 => numpad_minus(),
        75 => numpad_digit(4),
        76 => numpad_digit(5),
        77 => numpad_digit(6),
        78 => numpad_plus(),
        79 => numpad_digit(1),
        80 => numpad_digit(2),
        81 => numpad_digit(3),
        82 => numpad_digit(0),
        83 => numpad_delete(),
        86 => Key::Unicode('\\'), // IntlBackslash: no distinct enigo key, closest is the backslash character
        87 => Key::F11,
        88 => Key::F12,
        96 => Key::Return, // KpReturn: enigo has no separate numpad-Enter key
        97 => Key::RControl,
        98 => numpad_divide(),
        99 => print_screen_key(),
        100 => Key::Alt, // AltGr: enigo has no distinct right-Alt/AltGr key
        102 => Key::Home,
        103 => Key::UpArrow,
        104 => Key::PageUp,
        105 => Key::LeftArrow,
        106 => Key::RightArrow,
        107 => Key::End,
        108 => Key::DownArrow,
        109 => Key::PageDown,
        110 => insert_key(),
        111 => Key::Delete,
        119 => pause_key(),
        125 => Key::Meta,
        126 => Key::Meta,
        464 => function_key(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_map_to_a_key() {
        for code in [1u32, 14, 28, 42, 57, 96, 97, 110, 464] {
            assert!(code_to_key(code).is_some(), "code {code} should map");
        }
    }

    #[test]
    fn unknown_high_codes_are_ignored() {
        assert_eq!(code_to_key(0x1_0005), None);
        assert_eq!(code_to_key(0x1_FFFF), None);
    }

    #[test]
    fn truly_unmapped_low_code_is_none() {
        assert_eq!(code_to_key(9999), None);
    }
}
