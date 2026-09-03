use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Server name (advertised via mDNS and sent to clients in `HelloAck`)
    pub server_name: String,

    /// Paired devices (fingerprint -> device info)
    pub paired_devices: HashMap<String, PairedDevice>,

    /// Hotkey configuration
    pub hotkey: HotkeyConfig,

    /// Auto-start forwarding when the first client pairs; stop when the last disconnects
    pub auto_forward: bool,

    /// Enable clipboard sync
    pub clipboard_sync: bool,

    /// File transfer directory
    pub transfer_dir: PathBuf,

    /// File transfer port
    pub file_transfer_port: u16,

    /// Pairing PIN. Falls back to `--pin`, then `KVM_PAIRING_PIN`, then a
    /// random PIN generated at startup when unset.
    pub pairing_pin: Option<String>,

    /// Address the control and file-transfer listeners bind to
    pub bind_address: String,

    /// Maximum accepted file size in bytes. 0 means unlimited.
    pub max_file_size: u64,

    /// Optional path to append audit log lines to
    pub audit_log: Option<PathBuf>,

    /// SOCKS5 proxy settings
    pub socks5: Socks5Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedDevice {
    pub name: String,
    pub fingerprint: String,
    pub paired_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub toggle_forward: String,
    pub show_panel: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            toggle_forward: "Ctrl+Alt+F".to_string(),
            show_panel: "Ctrl+Alt+K".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Socks5Settings {
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub allow_anonymous: bool,
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
            hotkey: HotkeyConfig::default(),
            auto_forward: false,
            clipboard_sync: true,
            transfer_dir,
            file_transfer_port: kvm_common::DEFAULT_FILE_PORT,
            pairing_pin: None,
            bind_address: "0.0.0.0".to_string(),
            max_file_size: 0,
            audit_log: None,
            socks5: Socks5Settings::default(),
        }
    }
}

impl Config {
    /// Default config file location: `<config_dir>/kvm-rs/server.toml`.
    pub fn default_path() -> Result<PathBuf> {
        let dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine config directory"))?;
        Ok(dir.join("kvm-rs").join("server.toml"))
    }

    /// Loads the config at `path` if it exists (missing fields fall back to
    /// their defaults via `#[serde(default)]`); otherwise returns
    /// `Config::default()`.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("reading config file {}", path.display()))?;
            let config: Config = toml::from_str(&content)
                .with_context(|| format!("parsing config file {}", path.display()))?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    /// Writes the config, then restricts it to the current user - this file
    /// holds plaintext secrets (`pairing_pin`, `socks5.password`, and every
    /// paired device's fingerprint, itself a de-facto bearer credential once
    /// paired). Restriction failure is logged but not fatal: the config is
    /// still written and usable, just not as tightly locked down as it
    /// should be (e.g. on a filesystem that doesn't support the attempted
    /// permission model).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config directory {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)
            .with_context(|| format!("writing config file {}", path.display()))?;
        if let Err(e) = crate::tls::restrict_file_to_owner(path) {
            tracing::warn!(
                "Could not restrict permissions on config file {}: {}",
                path.display(),
                e
            );
        }
        Ok(())
    }

    /// Saves this config to the default config path.
    pub fn save_default(&self) -> Result<()> {
        let path = Self::default_path()?;
        self.save(&path)
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

    pub fn remove_paired_device(&mut self, fingerprint: &str) -> bool {
        self.paired_devices.remove(fingerprint).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partial_toml_loads_with_defaults() {
        let dir = std::env::temp_dir().join(format!("kvm_cfg_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server.toml");
        // A minimal config, like the README example - only sets a couple of fields.
        std::fs::write(
            &path,
            "server_name = \"MyServer\"\nclipboard_sync = false\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(&path).unwrap();
        assert_eq!(cfg.server_name, "MyServer");
        assert!(!cfg.clipboard_sync);
        // Everything else should fall back to defaults.
        assert_eq!(cfg.bind_address, "0.0.0.0");
        assert_eq!(cfg.max_file_size, 0);
        assert!(cfg.pairing_pin.is_none());
        assert_eq!(cfg.hotkey.toggle_forward, "Ctrl+Alt+F");
        assert!(!cfg.socks5.allow_anonymous);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_missing_file_returns_default() {
        let path =
            std::env::temp_dir().join(format!("kvm_cfg_missing_{}.toml", uuid::Uuid::new_v4()));
        let cfg = Config::load_or_default(&path).unwrap();
        assert_eq!(cfg.bind_address, "0.0.0.0");
    }

    #[test]
    fn test_save_and_reload_roundtrip() {
        let dir = std::env::temp_dir().join(format!("kvm_cfg_roundtrip_{}", uuid::Uuid::new_v4()));
        let path = dir.join("nested").join("server.toml");

        let mut cfg = Config::default();
        cfg.add_paired_device("abc123".to_string(), "My Laptop".to_string());
        cfg.pairing_pin = Some("123456".to_string());
        cfg.save(&path).unwrap();

        let reloaded = Config::load_or_default(&path).unwrap();
        assert!(reloaded.is_paired("abc123"));
        assert_eq!(reloaded.pairing_pin.as_deref(), Some("123456"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
