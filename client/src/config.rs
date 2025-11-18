use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Last connected server
    pub last_server: Option<String>,

    /// Known servers (address -> server info)
    pub known_servers: HashMap<String, KnownServer>,

    /// Auto-connect on startup
    pub auto_connect: bool,

    /// File receive directory
    pub receive_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownServer {
    pub name: String,
    pub address: String,
    pub fingerprint: String,
    pub last_connected: chrono::DateTime<chrono::Utc>,
}

impl Default for Config {
    fn default() -> Self {
        let receive_dir = dirs::download_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("kvm-received");

        Self {
            last_server: None,
            known_servers: HashMap::new(),
            auto_connect: false,
            receive_dir,
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
}
