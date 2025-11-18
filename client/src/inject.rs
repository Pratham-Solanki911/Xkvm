use anyhow::Result;
use enigo::{Button, Enigo, Key, KeyboardControllable, MouseButton as EnigoMouseButton, MouseControllable};
use kvm_common::{Event, MouseButton};
use std::sync::Mutex;
use tracing::{debug, warn};

pub struct InputInjector {
    enigo: Mutex<Enigo>,
}

impl InputInjector {
    pub fn new() -> Self {
        Self {
            enigo: Mutex::new(Enigo::new()),
        }
    }

    pub fn inject(&self, event: Event) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();

        match event {
            Event::MouseDelta { dx, dy } => {
                enigo.mouse_move_relative(dx, dy);
            }
            Event::MouseButtonPress(button) => {
                let enigo_button = convert_mouse_button(button);
                enigo.mouse_down(enigo_button);
            }
            Event::MouseButtonRelease(button) => {
                let enigo_button = convert_mouse_button(button);
                enigo.mouse_up(enigo_button);
            }
            Event::Wheel { delta_x, delta_y } => {
                // Enigo doesn't have direct wheel support in older versions
                // This is a limitation we'll note
                debug!("Wheel event: dx={}, dy={} (not fully supported)", delta_x, delta_y);
            }
            Event::KeyPress { code, modifiers } => {
                if let Some(key) = code_to_key(code) {
                    enigo.key_down(key);
                } else {
                    warn!("Unknown key code: {}", code);
                }
            }
            Event::KeyRelease { code, modifiers } => {
                if let Some(key) = code_to_key(code) {
                    enigo.key_up(key);
                } else {
                    warn!("Unknown key code: {}", code);
                }
            }
        }

        Ok(())
    }
}

fn convert_mouse_button(button: MouseButton) -> EnigoMouseButton {
    match button {
        MouseButton::Left => EnigoMouseButton::Left,
        MouseButton::Right => EnigoMouseButton::Right,
        MouseButton::Middle => EnigoMouseButton::Middle,
        MouseButton::Back => EnigoMouseButton::Back,
        MouseButton::Forward => EnigoMouseButton::Forward,
    }
}

fn code_to_key(code: u32) -> Option<Key> {
    // Map code back to Enigo keys
    // This is a simplified reverse mapping from the server's key_to_code
    match code {
        1 => Some(Key::Escape),
        2 => Some(Key::Layout('1')),
        3 => Some(Key::Layout('2')),
        4 => Some(Key::Layout('3')),
        5 => Some(Key::Layout('4')),
        6 => Some(Key::Layout('5')),
        7 => Some(Key::Layout('6')),
        8 => Some(Key::Layout('7')),
        9 => Some(Key::Layout('8')),
        10 => Some(Key::Layout('9')),
        11 => Some(Key::Layout('0')),
        12 => Some(Key::Layout('-')),
        13 => Some(Key::Layout('=')),
        14 => Some(Key::Backspace),
        15 => Some(Key::Tab),
        16 => Some(Key::Layout('q')),
        17 => Some(Key::Layout('w')),
        18 => Some(Key::Layout('e')),
        19 => Some(Key::Layout('r')),
        20 => Some(Key::Layout('t')),
        21 => Some(Key::Layout('y')),
        22 => Some(Key::Layout('u')),
        23 => Some(Key::Layout('i')),
        24 => Some(Key::Layout('o')),
        25 => Some(Key::Layout('p')),
        26 => Some(Key::Layout('[')),
        27 => Some(Key::Layout(']')),
        28 => Some(Key::Return),
        29 => Some(Key::Control),
        30 => Some(Key::Layout('a')),
        31 => Some(Key::Layout('s')),
        32 => Some(Key::Layout('d')),
        33 => Some(Key::Layout('f')),
        34 => Some(Key::Layout('g')),
        35 => Some(Key::Layout('h')),
        36 => Some(Key::Layout('j')),
        37 => Some(Key::Layout('k')),
        38 => Some(Key::Layout('l')),
        39 => Some(Key::Layout(';')),
        40 => Some(Key::Layout('\'')),
        41 => Some(Key::Layout('`')),
        42 => Some(Key::Shift),
        43 => Some(Key::Layout('\\')),
        44 => Some(Key::Layout('z')),
        45 => Some(Key::Layout('x')),
        46 => Some(Key::Layout('c')),
        47 => Some(Key::Layout('v')),
        48 => Some(Key::Layout('b')),
        49 => Some(Key::Layout('n')),
        50 => Some(Key::Layout('m')),
        51 => Some(Key::Layout(',')),
        52 => Some(Key::Layout('.')),
        53 => Some(Key::Layout('/')),
        54 => Some(Key::Shift),
        56 => Some(Key::Alt),
        57 => Some(Key::Space),
        58 => Some(Key::CapsLock),
        59 => Some(Key::F1),
        60 => Some(Key::F2),
        61 => Some(Key::F3),
        62 => Some(Key::F4),
        63 => Some(Key::F5),
        64 => Some(Key::F6),
        65 => Some(Key::F7),
        66 => Some(Key::F8),
        67 => Some(Key::F9),
        68 => Some(Key::F10),
        87 => Some(Key::F11),
        88 => Some(Key::F12),
        102 => Some(Key::Home),
        103 => Some(Key::UpArrow),
        104 => Some(Key::PageUp),
        105 => Some(Key::LeftArrow),
        106 => Some(Key::RightArrow),
        107 => Some(Key::End),
        108 => Some(Key::DownArrow),
        109 => Some(Key::PageDown),
        110 => Some(Key::Insert),
        111 => Some(Key::Delete),
        125 => Some(Key::Meta),
        126 => Some(Key::Meta),
        _ => None,
    }
}
