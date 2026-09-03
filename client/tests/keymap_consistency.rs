//! Cross-crate contract test: every key in the canonical evdev-style table
//! (see PATCH_PLAN.md) must survive a round trip through the server's
//! `key_to_code` and the client's `code_to_key`. This is the one place that
//! actually proves the two crates agree, since nothing else links them
//! together.

use kvm_server::capture::key_to_code;

/// Every `rdev::Key` variant listed in the canonical table, in the same
/// order it's documented. `Unknown(n)` is deliberately not included here:
/// per the table, codes derived from it (`0x1_0000 + n`) are logged at
/// debug and ignored by the client, not mapped to a key.
fn canonical_keys() -> Vec<rdev::Key> {
    use rdev::Key::*;
    vec![
        Escape,
        Num1,
        Num2,
        Num3,
        Num4,
        Num5,
        Num6,
        Num7,
        Num8,
        Num9,
        Num0,
        Minus,
        Equal,
        Backspace,
        Tab,
        KeyQ,
        KeyW,
        KeyE,
        KeyR,
        KeyT,
        KeyY,
        KeyU,
        KeyI,
        KeyO,
        KeyP,
        LeftBracket,
        RightBracket,
        Return,
        ControlLeft,
        KeyA,
        KeyS,
        KeyD,
        KeyF,
        KeyG,
        KeyH,
        KeyJ,
        KeyK,
        KeyL,
        SemiColon,
        Quote,
        BackQuote,
        ShiftLeft,
        BackSlash,
        KeyZ,
        KeyX,
        KeyC,
        KeyV,
        KeyB,
        KeyN,
        KeyM,
        Comma,
        Dot,
        Slash,
        ShiftRight,
        KpMultiply,
        Alt,
        Space,
        CapsLock,
        F1,
        F2,
        F3,
        F4,
        F5,
        F6,
        F7,
        F8,
        F9,
        F10,
        NumLock,
        ScrollLock,
        Kp7,
        Kp8,
        Kp9,
        KpMinus,
        Kp4,
        Kp5,
        Kp6,
        KpPlus,
        Kp1,
        Kp2,
        Kp3,
        Kp0,
        KpDelete,
        IntlBackslash,
        F11,
        F12,
        KpReturn,
        ControlRight,
        KpDivide,
        PrintScreen,
        AltGr,
        Home,
        UpArrow,
        PageUp,
        LeftArrow,
        RightArrow,
        End,
        DownArrow,
        PageDown,
        Insert,
        Delete,
        Pause,
        MetaLeft,
        MetaRight,
        Function,
    ]
}

#[test]
fn every_canonical_key_round_trips_to_some_client_key() {
    for key in canonical_keys() {
        let code = key_to_code(key);
        assert_ne!(code, 0, "{key:?} should have a nonzero canonical code");
        let mapped = kvm_client::inject::code_to_key(code);
        assert!(
            mapped.is_some(),
            "{key:?} -> code {code} should map back to an enigo::Key on this platform"
        );
    }
}

#[test]
fn canonical_codes_are_unique() {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for key in canonical_keys() {
        let code = key_to_code(key);
        assert!(
            seen.insert(code),
            "duplicate canonical code {code} for {key:?}"
        );
    }
}

#[test]
fn unknown_key_code_is_ignored_by_the_client() {
    // Per the table: `Unknown(n) => 0x1_0000 + n`, and the client logs it at
    // debug and ignores it rather than mapping it to a key.
    let code = key_to_code(rdev::Key::Unknown(5));
    assert_eq!(code, 0x1_0005);
    assert_eq!(kvm_client::inject::code_to_key(code), None);
}
