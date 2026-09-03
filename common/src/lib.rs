use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub mod display;

/// Wire protocol version. Bumped whenever the `Envelope` layout changes.
pub const PROTOCOL_VERSION: &str = "0.2.0";
pub const DEFAULT_CONTROL_PORT: u16 = 4000;
pub const DEFAULT_FILE_PORT: u16 = 4001;
pub const MDNS_SERVICE_TYPE: &str = "_kvm-rs._tcp.local.";
/// Upper bound for a single framed message.
pub const MAX_ENVELOPE_SIZE: usize = 10 * 1024 * 1024;

/// Capabilities that a server can advertise
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Caps {
    pub clipboard: bool,
    pub file_transfer: bool,
    pub internet_share: bool,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            clipboard: true,
            file_transfer: true,
            internet_share: false,
        }
    }
}

/// Main envelope for all protocol messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Envelope {
    /// Initial handshake from client to server
    Hello {
        name: String,
        version: String,
        caps: Caps,
    },
    /// Server acknowledges the hello
    HelloAck {
        file_transfer_port: u16,
        server_name: String,
    },
    /// Client requests pairing / authenticates.
    ///
    /// `fingerprint` is the client's stable identity (hex SHA-256 of a secret
    /// stored in the client config). `proof` is
    /// [`pairing_proof`]`(pin, nonce, fingerprint)`; the server checks it
    /// against its pairing PIN when the fingerprint is not yet paired.
    PairRequest {
        nonce: [u8; 16],
        fingerprint: String,
        proof: [u8; 32],
    },
    /// Server accepts pairing
    PairAccept,
    /// Server rejects pairing
    PairReject { reason: String },
    /// Toggle input forwarding on/off (client -> server request, or server -> client notice)
    ToggleForward(bool),
    /// Input event (keyboard/mouse)
    Input(Event),
    /// Set clipboard content
    ClipboardSet { text: String },
    /// Offer a file for transfer
    FileOffer {
        id: Uuid,
        name: String,
        size: u64,
        sha256: [u8; 32],
    },
    /// Accept file transfer (with optional resume offset)
    FileAccept { id: Uuid, start_offset: u64 },
    /// Reject file transfer
    FileReject { id: Uuid, reason: String },
    /// File transfer complete notification
    FileComplete { id: Uuid },
    /// Ping for keepalive and latency measurement
    Ping(u64),
    /// Pong response
    Pong(u64),
    /// Error message
    Error(String),
    /// Graceful disconnect
    Goodbye,
}

/// Input events (keyboard and mouse)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    /// Relative mouse movement (dx, dy)
    MouseDelta { dx: i32, dy: i32 },
    /// Mouse button press
    MouseButtonPress(MouseButton),
    /// Mouse button release
    MouseButtonRelease(MouseButton),
    /// Mouse wheel scroll
    Wheel { delta_x: i32, delta_y: i32 },
    /// Key press with scancode and modifiers
    KeyPress { code: u32, modifiers: u8 },
    /// Key release with scancode and modifiers
    KeyRelease { code: u32, modifiers: u8 },
    /// Release every key and mouse button currently held on the receiving side.
    /// Sent when forwarding is switched off or a session ends so nothing stays stuck.
    ReleaseAll,
}

/// Mouse buttons
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Modifier keys bitmask
pub mod modifiers {
    pub const CTRL: u8 = 1 << 0;
    pub const ALT: u8 = 1 << 1;
    pub const SHIFT: u8 = 1 << 2;
    pub const META: u8 = 1 << 3;
}

/// Error types for the protocol
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),

    #[error("Invalid message")]
    InvalidMessage,

    #[error("Protocol version mismatch")]
    VersionMismatch,

    #[error("Pairing rejected: {0}")]
    PairingRejected(String),

    #[error("Connection closed")]
    ConnectionClosed,
}

pub type Result<T> = std::result::Result<T, ProtocolError>;

/// Compute the pairing proof sent in [`Envelope::PairRequest`].
///
/// `SHA-256(pin || 0x00 || nonce || 0x00 || fingerprint)`. The PIN is never
/// sent on the wire; the server recomputes the proof with its own PIN.
pub fn pairing_proof(pin: &str, nonce: &[u8; 16], fingerprint: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(pin.as_bytes());
    hasher.update([0u8]);
    hasher.update(nonce);
    hasher.update([0u8]);
    hasher.update(fingerprint.as_bytes());
    hasher.finalize().into()
}

/// Constant-time comparison of two proofs.
pub fn proof_matches(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Serialize an envelope to bytes with length prefix
pub fn serialize_envelope(envelope: &Envelope) -> Result<Vec<u8>> {
    let data = bincode::serialize(envelope)?;
    if data.len() > MAX_ENVELOPE_SIZE {
        return Err(ProtocolError::InvalidMessage);
    }
    let len = data.len() as u32;
    let mut result = Vec::with_capacity(4 + data.len());
    result.extend_from_slice(&len.to_be_bytes());
    result.extend_from_slice(&data);
    Ok(result)
}

/// Deserialize an envelope from bytes (without length prefix)
pub fn deserialize_envelope(data: &[u8]) -> Result<Envelope> {
    bincode::deserialize(data).map_err(Into::into)
}

pub async fn read_envelope<S>(stream: &mut S) -> Result<Envelope>
where
    S: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > MAX_ENVELOPE_SIZE {
        return Err(ProtocolError::InvalidMessage);
    }

    let mut data = vec![0u8; len];
    stream.read_exact(&mut data).await?;

    deserialize_envelope(&data)
}

pub async fn send_envelope<S>(stream: &mut S, envelope: &Envelope) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    let data = serialize_envelope(envelope)?;
    stream.write_all(&data).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_serialization() {
        let envelope = Envelope::Hello {
            name: "test".to_string(),
            version: PROTOCOL_VERSION.to_string(),
            caps: Caps::default(),
        };

        let serialized = serialize_envelope(&envelope).unwrap();
        assert!(serialized.len() > 4); // Has length prefix

        let len = u32::from_be_bytes([serialized[0], serialized[1], serialized[2], serialized[3]]);
        let deserialized = deserialize_envelope(&serialized[4..]).unwrap();

        assert_eq!(len as usize, serialized.len() - 4);
        assert!(matches!(deserialized, Envelope::Hello { .. }));
    }

    #[test]
    fn test_event_serialization() {
        let event = Event::MouseDelta { dx: 10, dy: -5 };
        let envelope = Envelope::Input(event);

        let serialized = serialize_envelope(&envelope).unwrap();
        let deserialized = deserialize_envelope(&serialized[4..]).unwrap();

        assert!(matches!(
            deserialized,
            Envelope::Input(Event::MouseDelta { dx: 10, dy: -5 })
        ));
    }

    #[test]
    fn test_empty_and_unicode_clipboard_envelope() {
        let empty_envelope = Envelope::ClipboardSet {
            text: "".to_string(),
        };
        let ser_empty = serialize_envelope(&empty_envelope).unwrap();
        let de_empty = deserialize_envelope(&ser_empty[4..]).unwrap();
        if let Envelope::ClipboardSet { text } = de_empty {
            assert_eq!(text, "");
        } else {
            panic!("Expected ClipboardSet");
        }

        let unicode_envelope = Envelope::ClipboardSet {
            text: "🚀 KVM-RS Unicode Test ⚡ ⚡ ⚡".to_string(),
        };
        let ser_unicode = serialize_envelope(&unicode_envelope).unwrap();
        let de_unicode = deserialize_envelope(&ser_unicode[4..]).unwrap();
        if let Envelope::ClipboardSet { text } = de_unicode {
            assert_eq!(text, "🚀 KVM-RS Unicode Test ⚡ ⚡ ⚡");
        } else {
            panic!("Expected ClipboardSet");
        }
    }

    #[tokio::test]
    async fn test_read_envelope_oversized_limit() {
        // Construct 4-byte length prefix specifying 15 MB (> 10MB limit)
        let oversized_len: u32 = 15 * 1024 * 1024;
        let mut mock_stream = std::io::Cursor::new(oversized_len.to_be_bytes());
        let result = read_envelope(&mut mock_stream).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_oversized_envelope_rejected() {
        let text = "x".repeat(MAX_ENVELOPE_SIZE + 1);
        let result = serialize_envelope(&Envelope::ClipboardSet { text });
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_corrupted_data() {
        let garbage = [0xFE, 0xED, 0xFA, 0xCE, 0x12, 0x34];
        let result = deserialize_envelope(&garbage);
        assert!(result.is_err());
    }

    #[test]
    fn test_pairing_proof_is_deterministic_and_pin_sensitive() {
        let nonce = [7u8; 16];
        let a = pairing_proof("123456", &nonce, "fp");
        let b = pairing_proof("123456", &nonce, "fp");
        let c = pairing_proof("654321", &nonce, "fp");
        let d = pairing_proof("123456", &[8u8; 16], "fp");
        let e = pairing_proof("123456", &nonce, "other");
        assert!(proof_matches(&a, &b));
        assert!(!proof_matches(&a, &c));
        assert!(!proof_matches(&a, &d));
        assert!(!proof_matches(&a, &e));
    }
}
