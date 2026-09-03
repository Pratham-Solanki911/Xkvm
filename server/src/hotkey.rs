//! Parses a "Ctrl+Alt+F"-style hotkey string into a [`HotkeyChord`] that the
//! input-capture callback can match against currently-held keys. This
//! replaces the `global-hotkey` crate, whose OS-level registration silently
//! never fires on Windows/macOS in this codebase's setup; detecting the
//! chord inside the capture callback (which sees every key regardless of
//! forwarding state) works everywhere.

use anyhow::{anyhow, Result};
use rdev::Key;

/// A parsed hotkey chord: a set of required modifiers plus one non-modifier
/// trigger key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyChord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: Key,
}

/// Parses strings like `"Ctrl+Alt+F"` (case-insensitive, whitespace-tolerant).
pub fn parse_hotkey(s: &str) -> Result<HotkeyChord> {
    let parts: Vec<&str> = s
        .split('+')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    if parts.is_empty() {
        return Err(anyhow!("empty hotkey string"));
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut meta = false;
    let mut key: Option<Key> = None;

    for part in &parts {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "super" | "win" | "meta" | "cmd" => meta = true,
            other => {
                if key.is_some() {
                    return Err(anyhow!("multiple key codes in hotkey string: {}", s));
                }
                key = Some(parse_key_name(other)?);
            }
        }
    }

    let key = key.ok_or_else(|| anyhow!("no key code found in hotkey string: {}", s))?;

    Ok(HotkeyChord {
        ctrl,
        alt,
        shift,
        meta,
        key,
    })
}

fn parse_key_name(s: &str) -> Result<Key> {
    let upper = s.to_uppercase();

    if upper.len() == 1 {
        let c = upper.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Ok(match c {
                'A' => Key::KeyA,
                'B' => Key::KeyB,
                'C' => Key::KeyC,
                'D' => Key::KeyD,
                'E' => Key::KeyE,
                'F' => Key::KeyF,
                'G' => Key::KeyG,
                'H' => Key::KeyH,
                'I' => Key::KeyI,
                'J' => Key::KeyJ,
                'K' => Key::KeyK,
                'L' => Key::KeyL,
                'M' => Key::KeyM,
                'N' => Key::KeyN,
                'O' => Key::KeyO,
                'P' => Key::KeyP,
                'Q' => Key::KeyQ,
                'R' => Key::KeyR,
                'S' => Key::KeyS,
                'T' => Key::KeyT,
                'U' => Key::KeyU,
                'V' => Key::KeyV,
                'W' => Key::KeyW,
                'X' => Key::KeyX,
                'Y' => Key::KeyY,
                'Z' => Key::KeyZ,
                _ => unreachable!(),
            });
        }
        if c.is_ascii_digit() {
            return Ok(match c {
                '0' => Key::Num0,
                '1' => Key::Num1,
                '2' => Key::Num2,
                '3' => Key::Num3,
                '4' => Key::Num4,
                '5' => Key::Num5,
                '6' => Key::Num6,
                '7' => Key::Num7,
                '8' => Key::Num8,
                '9' => Key::Num9,
                _ => unreachable!(),
            });
        }
    }

    match upper.as_str() {
        "F1" => Ok(Key::F1),
        "F2" => Ok(Key::F2),
        "F3" => Ok(Key::F3),
        "F4" => Ok(Key::F4),
        "F5" => Ok(Key::F5),
        "F6" => Ok(Key::F6),
        "F7" => Ok(Key::F7),
        "F8" => Ok(Key::F8),
        "F9" => Ok(Key::F9),
        "F10" => Ok(Key::F10),
        "F11" => Ok(Key::F11),
        "F12" => Ok(Key::F12),
        "SPACE" => Ok(Key::Space),
        "ESCAPE" | "ESC" => Ok(Key::Escape),
        "TAB" => Ok(Key::Tab),
        "ENTER" | "RETURN" => Ok(Key::Return),
        "UP" | "UPARROW" => Ok(Key::UpArrow),
        "DOWN" | "DOWNARROW" => Ok(Key::DownArrow),
        "LEFT" | "LEFTARROW" => Ok(Key::LeftArrow),
        "RIGHT" | "RIGHTARROW" => Ok(Key::RightArrow),
        "HOME" => Ok(Key::Home),
        "END" => Ok(Key::End),
        "PAGEUP" => Ok(Key::PageUp),
        "PAGEDOWN" => Ok(Key::PageDown),
        "INSERT" => Ok(Key::Insert),
        "DELETE" | "DEL" => Ok(Key::Delete),
        "BACKSPACE" => Ok(Key::Backspace),
        "PAUSE" => Ok(Key::Pause),
        "SCROLLLOCK" => Ok(Key::ScrollLock),
        "PRINTSCREEN" | "PRTSC" => Ok(Key::PrintScreen),
        "BACKQUOTE" | "GRAVE" | "`" => Ok(Key::BackQuote),
        _ => Err(anyhow!("unknown key code: {}", s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_simple() {
        let chord = parse_hotkey("Ctrl+Alt+F").unwrap();
        assert!(chord.ctrl);
        assert!(chord.alt);
        assert!(!chord.shift);
        assert!(!chord.meta);
        assert_eq!(chord.key, Key::KeyF);
    }

    #[test]
    fn test_parse_case_insensitive_and_whitespace() {
        let chord = parse_hotkey(" cTrL + sHiFt +  k ").unwrap();
        assert!(chord.ctrl);
        assert!(chord.shift);
        assert!(!chord.alt);
        assert_eq!(chord.key, Key::KeyK);
    }

    #[test]
    fn test_parse_function_key_and_meta_alias() {
        let chord = parse_hotkey("Super+F5").unwrap();
        assert!(chord.meta);
        assert_eq!(chord.key, Key::F5);
    }

    #[test]
    fn test_parse_digit_key() {
        let chord = parse_hotkey("Ctrl+3").unwrap();
        assert_eq!(chord.key, Key::Num3);
    }

    #[test]
    fn test_parse_named_key() {
        let chord = parse_hotkey("Ctrl+Alt+Escape").unwrap();
        assert_eq!(chord.key, Key::Escape);
    }

    #[test]
    fn test_parse_unknown_key_fails() {
        let err = parse_hotkey("Ctrl+Alt+NotAKey").unwrap_err();
        assert!(err.to_string().contains("unknown key code"));
    }

    #[test]
    fn test_parse_duplicate_key_fails() {
        let err = parse_hotkey("F+K").unwrap_err();
        assert!(err.to_string().contains("multiple key codes"));
    }

    #[test]
    fn test_parse_no_key_fails() {
        let err = parse_hotkey("Ctrl+Alt").unwrap_err();
        assert!(err.to_string().contains("no key code found"));
    }

    #[test]
    fn test_parse_empty_fails() {
        assert!(parse_hotkey("").is_err());
        assert!(parse_hotkey("   ").is_err());
    }

    #[test]
    fn test_parse_default_config_hotkeys() {
        // Both defaults from HotkeyConfig::default() must parse.
        assert!(parse_hotkey("Ctrl+Alt+F").is_ok());
        assert!(parse_hotkey("Ctrl+Alt+K").is_ok());
    }
}
