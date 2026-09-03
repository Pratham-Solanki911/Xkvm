//! Library half of the KVM-RS client: everything except the CLI wrapper in
//! `main.rs`. This is what the Tauri UI links against.

pub mod addr;
mod clipboard;
pub mod config;
pub mod discovery;
pub mod file_transfer;
pub mod inject;
pub mod session;
pub mod tls;

pub use config::Config;
pub use session::{
    run_with_reconnect, ConnectOptions, FatalConnectError, Session, SessionController, SessionEvent,
};
