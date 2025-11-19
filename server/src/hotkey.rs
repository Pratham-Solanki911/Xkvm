use anyhow::{anyhow, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

pub fn parse_hotkey(s: &str) -> Result<HotKey> {
    let parts: Vec<&str> = s.split('+').map(|s| s.trim()).collect();
    if parts.is_empty() {
        return Err(anyhow!("Empty hotkey string"));
    }

    let mut modifiers = Modifiers::empty();
    let mut key_code = None;

    for part in parts {
        match part.to_lowercase().as_str() {
            "ctrl" => modifiers |= Modifiers::CONTROL,
            "alt" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "super" | "win" | "meta" => modifiers |= Modifiers::SUPER,
            _ => {
                if key_code.is_some() {
                    return Err(anyhow!("Multiple key codes in hotkey string: {}", s));
                }
                key_code = Some(parse_key_code(part)?);
            }
        }
    }

    if let Some(code) = key_code {
        Ok(HotKey::new(Some(modifiers), code))
    } else {
        Err(anyhow!("No key code found in hotkey string: {}", s))
    }
}

fn parse_key_code(s: &str) -> Result<Code> {
    // This is a simplified mapping. A real implementation would be more robust.
    match s.to_uppercase().as_str() {
        "Q" => Ok(Code::KeyQ),
        "F" => Ok(Code::KeyF),
        "K" => Ok(Code::KeyK),
        _ => Err(anyhow!("Unknown key code: {}", s)),
    }
}
