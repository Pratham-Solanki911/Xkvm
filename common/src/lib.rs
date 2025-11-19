use serde::{Deserialize, Serialize};
use uuid::Uuid;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: &str = "0.1.0";
pub const DEFAULT_CONTROL_PORT: u16 = 4000;
pub const DEFAULT_FILE_PORT: u16 = 4001;
pub const MDNS_SERVICE_TYPE: &str = "_kvm-rs._tcp.local.";

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
    HelloAck { file_transfer_port: u16 },
    /// Client requests pairing
    PairRequest {
        nonce: [u8; 16],
        fingerprint: String,
    },
    /// Server accepts pairing
    PairAccept,
    /// Server rejects pairing
    PairReject { reason: String },
    /// Toggle input forwarding on/off
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Serialize an envelope to bytes with length prefix
pub fn serialize_envelope(envelope: &Envelope) -> Result<Vec<u8>> {
    let data = bincode::serialize(envelope)?;
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

    if len > 10 * 1024 * 1024 {
        anyhow::bail!("Message too large: {} bytes", len);
    }

    let mut data = vec![0u8; len];
    stream.read_exact(&mut data).await?;

    deserialize_envelope(&data).map_err(Into::into)
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
        matches!(deserialized, Envelope::Hello { .. });
    }

    #[test]
    fn test_event_serialization() {
        let event = Event::MouseDelta { dx: 10, dy: -5 };
        let envelope = Envelope::Input(event);

        let serialized = serialize_envelope(&envelope).unwrap();
        let deserialized = deserialize_envelope(&serialized[4..]).unwrap();

        matches!(deserialized, Envelope::Input(Event::MouseDelta { .. }));
    }
}
