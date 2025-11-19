use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Server name (advertised via mDNS)
    pub server_name: String,

    /// Paired devices (fingerprint -> device info)
    pub paired_devices: HashMap<String, PairedDevice>,

    /// Hotkey configuration
    pub hotkey: HotkeyConfig,

    /// Auto-start forwarding on connect
    pub auto_forward: bool,

    /// Enable clipboard sync
    pub clipboard_sync: bool,

    /// File transfer directory
    pub transfer_dir: PathBuf,

    /// File transfer port
    pub file_transfer_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedDevice {
    pub name: String,
    pub fingerprint: String,
    pub paired_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    pub toggle_forward: String,
    pub show_panel: String,
}

impl Default for Config {
    fn default() -> Self {
        let transfer_dir = dirs::download_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("kvm-transfers");

        Self {
            server_name: hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "KVM-Server".to_string()),
            paired_devices: HashMap::new(),
            hotkey: HotkeyConfig {
                toggle_forward: "Ctrl+Alt+F".to_string(),
                show_panel: "Ctrl+Alt+K".to_string(),
            },
            auto_forward: false,
            clipboard_sync: true,
            transfer_dir,
            file_transfer_port: kvm_common::DEFAULT_FILE_PORT,
        }
    }
}

impl Config {
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self, path: &str) -> Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn is_paired(&self, fingerprint: &str) -> bool {
        self.paired_devices.contains_key(fingerprint)
    }

    pub fn add_paired_device(&mut self, fingerprint: String, name: String) {
        self.paired_devices.insert(
            fingerprint.clone(),
            PairedDevice {
                name,
                fingerprint,
                paired_at: chrono::Utc::now(),
            },
        );
    }
}
