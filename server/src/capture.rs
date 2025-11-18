use kvm_common::{modifiers, Event, MouseButton};
use rdev::{Button, EventType, Key};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error};

pub struct InputCapture {
    enabled: Arc<AtomicBool>,
    last_x: Arc<std::sync::Mutex<f64>>,
    last_y: Arc<std::sync::Mutex<f64>>,
}

impl InputCapture {
    pub fn new() -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(false)),
            last_x: Arc::new(std::sync::Mutex::new(0.0)),
            last_y: Arc::new(std::sync::Mutex::new(0.0)),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn start(&self, tx: mpsc::UnboundedSender<Event>) {
        let enabled = self.enabled.clone();
        let last_x = self.last_x.clone();
        let last_y = self.last_y.clone();

        std::thread::spawn(move || {
            if let Err(e) = rdev::listen(move |event| {
                if !enabled.load(Ordering::SeqCst) {
                    return;
                }

                if let Some(kvm_event) = convert_event(&event, &last_x, &last_y) {
                    if tx.send(kvm_event).is_err() {
                        error!("Failed to send captured event");
                    }
                }
            }) {
                error!("Failed to listen to input events: {:?}", e);
            }
        });
    }
}

fn convert_event(
    event: &rdev::Event,
    last_x: &Arc<std::sync::Mutex<f64>>,
    last_y: &Arc<std::sync::Mutex<f64>>,
) -> Option<Event> {
    match event.event_type {
        EventType::MouseMove { x, y } => {
            let mut last_pos = last_x.lock().unwrap();
            let mut last_pos_y = last_y.lock().unwrap();

            let dx = (x - *last_pos) as i32;
            let dy = (y - *last_pos_y) as i32;

            *last_pos = x;
            *last_pos_y = y;

            if dx != 0 || dy != 0 {
                Some(Event::MouseDelta { dx, dy })
            } else {
                None
            }
        }
        EventType::ButtonPress(button) => {
            Some(Event::MouseButtonPress(convert_mouse_button(button)))
        }
        EventType::ButtonRelease(button) => {
            Some(Event::MouseButtonRelease(convert_mouse_button(button)))
        }
        EventType::Wheel { delta_x, delta_y } => Some(Event::Wheel {
            delta_x: delta_x as i32,
            delta_y: delta_y as i32,
        }),
        EventType::KeyPress(key) => {
            let (code, mods) = convert_key(key);
            Some(Event::KeyPress {
                code,
                modifiers: mods,
            })
        }
        EventType::KeyRelease(key) => {
            let (code, mods) = convert_key(key);
            Some(Event::KeyRelease {
                code,
                modifiers: mods,
            })
        }
    }
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

fn convert_key(key: Key) -> (u32, u8) {
    let mut modifiers = 0u8;

    // Extract modifiers
    match key {
        Key::ControlLeft | Key::ControlRight => modifiers |= modifiers::CTRL,
        Key::Alt | Key::AltGr => modifiers |= modifiers::ALT,
        Key::ShiftLeft | Key::ShiftRight => modifiers |= modifiers::SHIFT,
        Key::MetaLeft | Key::MetaRight => modifiers |= modifiers::META,
        _ => {}
    }

    // Convert key to scancode-like value
    let code = key_to_code(key);

    (code, modifiers)
}

fn key_to_code(key: Key) -> u32 {
    // Map rdev::Key to a numeric code
    // This is a simplified mapping; ideally use platform-specific scancodes
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
        Key::F11 => 87,
        Key::F12 => 88,
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
        Key::MetaLeft => 125,
        Key::MetaRight => 126,
        _ => 0, // Unknown key
    }
}
