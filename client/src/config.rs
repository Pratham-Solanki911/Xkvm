use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Last connected server address (`host:port`, IPv6 bracketed).
    pub last_server: Option<String>,

    /// Known servers, keyed by the same address string used to connect.
    pub known_servers: HashMap<String, KnownServer>,

    /// Auto-connect on startup (reserved for the UI).
    pub auto_connect: bool,

    /// This client's stable identity: 32 random bytes, hex-encoded.
    /// Generated once on first run via [`Config::ensure_client_secret`] and
    /// persisted from then on. The client's fingerprint sent in
    /// `PairRequest` is `SHA-256(secret)`.
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownServer {
    pub name: String,
    pub address: String,
    pub fingerprint: String,
    pub last_connected: chrono::DateTime<chrono::Utc>,
}

impl Default for KnownServer {
    fn default() -> Self {
        Self {
            name: String::new(),
            address: String::new(),
            fingerprint: String::new(),
            last_connected: chrono::Utc::now(),
        }
    }
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
            .join("kvm-rs");
        std::fs::create_dir_all(&config_dir)?;
        Ok(config_dir.join("client.toml"))
    }

    /// Load `path`, falling back to defaults if it doesn't exist yet. If it
    /// exists but fails to parse, the broken file is renamed to
    /// `<name>.bak` (never silently overwritten) and a fresh default config
    /// is returned.
    pub fn load_or_default(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };

        match toml::from_str(&content) {
            Ok(config) => config,
            Err(e) => {
                warn!(
                    "failed to parse client config at {}: {} -- backing it up and starting fresh",
                    path.display(),
                    e
                );
                let backup = path.with_extension("toml.bak");
                if let Err(e) = std::fs::rename(path, &backup) {
                    warn!(
                        "failed to back up broken config to {}: {}",
                        backup.display(),
                        e
                    );
                }
                Self::default()
            }
        }
    }

    /// Writes the config, then restricts it to the current user only. The
    /// file holds `client_secret`, whose SHA-256 (`client_fingerprint()`) is
    /// a bearer credential once paired (a known fingerprint skips proof
    /// verification on the server), so it deserves the same owner-only
    /// permissions the server already applies to its own equivalently
    /// sensitive files (`server.key`, `server.toml`, the audit log). The
    /// permission step is best-effort: a failure is logged but does not
    /// prevent the config from being saved.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        if let Err(e) = restrict_file_to_owner(path) {
            warn!(
                "could not restrict client config {} to the current user: {}",
                path.display(),
                e
            );
        }
        Ok(())
    }

    pub fn save_default(&self) -> Result<()> {
        self.save(&Self::default_path()?)
    }

    /// Generate `client_secret` if it isn't set yet. Returns `true` if a new
    /// secret was generated, so the caller knows to persist the config.
    pub fn ensure_client_secret(&mut self) -> bool {
        if self.client_secret.is_empty() {
            let bytes: [u8; 32] = rand::random();
            self.client_secret = hex_encode(&bytes);
            true
        } else {
            false
        }
    }

    /// This client's stable fingerprint: hex `SHA-256(client_secret bytes)`.
    /// [`ensure_client_secret`](Self::ensure_client_secret) should be called
    /// first; an empty secret still yields a (useless but harmless) hash.
    pub fn client_fingerprint(&self) -> String {
        let bytes = hex_decode(&self.client_secret).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    }

    pub fn add_known_server(&mut self, name: String, address: String, fingerprint: String) {
        self.last_server = Some(address.clone());
        self.known_servers.insert(
            address.clone(),
            KnownServer {
                name,
                address,
                fingerprint,
                last_connected: chrono::Utc::now(),
            },
        );
    }

    pub fn known_fingerprint(&self, address: &str) -> Option<String> {
        self.known_servers
            .get(address)
            .map(|s| s.fingerprint.clone())
    }

    /// Record the same server's fingerprint under an additional address
    /// (e.g. its file-transfer port, which serves the same TLS certificate
    /// as the control port) without disturbing `last_server` or any other
    /// entry. Used so a lookup keyed by the file-transfer address (as
    /// [`crate::file_transfer::send_file`] does) finds the fingerprint that
    /// was actually observed and pinned on the control channel.
    pub fn add_known_server_alias(&mut self, name: String, address: String, fingerprint: String) {
        self.known_servers.insert(
            address.clone(),
            KnownServer {
                name,
                address,
                fingerprint,
                last_connected: chrono::Utc::now(),
            },
        );
    }

    /// Known servers sorted by most-recently-connected first.
    pub fn list_known_servers(&self) -> Vec<KnownServer> {
        let mut servers: Vec<_> = self.known_servers.values().cloned().collect();
        servers.sort_by_key(|s| std::cmp::Reverse(s.last_connected));
        servers
    }

    /// Remove a known server. Returns `true` if it was present.
    pub fn forget_server(&mut self, address: &str) -> bool {
        self.known_servers.remove(address).is_some()
    }
}

/// Restricts `path` to the current user only, best-effort on the caller's
/// behalf but reported as an error so callers can decide whether it's fatal
/// (here, it isn't -- see [`Config::save`]). Unix: `chmod 0600`. Windows:
/// replace inherited ACLs with a single full-control entry for the current
/// user via `icacls` (no extra crate needed). This is the client-side
/// counterpart of the server's `restrict_file_to_owner` in
/// `server/src/tls.rs` (kept as a separate copy since the client crate does
/// not depend on the server crate).
fn restrict_file_to_owner(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        let username = std::env::var("USERNAME").map_err(|_| {
            anyhow::anyhow!("could not determine current username (USERNAME unset)")
        })?;
        let status = std::process::Command::new("icacls")
            .arg(path)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(format!("{}:F", username))
            .status()
            .map_err(|e| anyhow::anyhow!("failed to run icacls: {}", e))?;
        if !status.success() {
            anyhow::bail!("icacls exited with status {}", status);
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode a hex string into bytes. Operates on raw bytes (not `&str`
/// slicing) so a `client_secret` containing multi-byte UTF-8 characters
/// (e.g. hand-edited or corrupted config) can never panic on a
/// non-char-boundary slice: any byte that isn't an ASCII hex digit simply
/// fails to parse and the whole decode returns `None`.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    (0..bytes.len())
        .step_by(2)
        .map(|i| {
            let hi = (bytes[i] as char).to_digit(16)?;
            let lo = (bytes[i + 1] as char).to_digit(16)?;
            Some(((hi << 4) | lo) as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile_shim::TempDir;

    // Minimal temp-dir helper so this crate doesn't need a `tempfile`
    // dev-dependency just for these tests.
    mod tempfile_shim {
        use std::path::PathBuf;

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new(tag: &str) -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "kvm-client-test-{tag}-{}-{}",
                    std::process::id(),
                    rand::random::<u64>()
                ));
                std::fs::create_dir_all(&dir).unwrap();
                Self(dir)
            }

            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn missing_file_loads_default() {
        let dir = TempDir::new("missing");
        let path = dir.path().join("client.toml");
        let config = Config::load_or_default(&path);
        assert!(config.client_secret.is_empty());
        assert!(config.known_servers.is_empty());
    }

    #[test]
    fn partial_toml_loads_via_defaults() {
        let dir = TempDir::new("partial");
        let path = dir.path().join("client.toml");
        std::fs::write(&path, "auto_connect = true\n").unwrap();
        let config = Config::load_or_default(&path);
        assert!(config.auto_connect);
        assert!(config.last_server.is_none());
        assert!(config.client_secret.is_empty());
    }

    #[test]
    fn broken_config_is_backed_up_not_overwritten() {
        let dir = TempDir::new("broken");
        let path = dir.path().join("client.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();
        let config = Config::load_or_default(&path);
        assert!(config.known_servers.is_empty());

        let backup = path.with_extension("toml.bak");
        assert!(
            backup.exists(),
            "broken config should be renamed to a .bak file"
        );
        assert!(
            !path.exists(),
            "original broken path should no longer exist"
        );
        let backed_up = std::fs::read_to_string(&backup).unwrap();
        assert!(backed_up.contains("not valid toml"));
    }

    #[test]
    fn ensure_client_secret_is_generated_once_and_stable() {
        let mut config = Config::default();
        assert!(config.ensure_client_secret());
        let secret = config.client_secret.clone();
        let fp = config.client_fingerprint();
        assert!(
            !config.ensure_client_secret(),
            "second call should not regenerate"
        );
        assert_eq!(config.client_secret, secret);
        assert_eq!(config.client_fingerprint(), fp);
        assert_eq!(fp.len(), 64, "sha256 hex digest should be 64 chars");
    }

    #[test]
    fn save_and_reload_round_trips() {
        let dir = TempDir::new("roundtrip");
        let path = dir.path().join("client.toml");
        let mut config = Config::default();
        config.ensure_client_secret();
        config.add_known_server("srv".into(), "127.0.0.1:4000".into(), "AB:CD".into());
        config.save(&path).unwrap();

        let reloaded = Config::load_or_default(&path);
        assert_eq!(reloaded.client_secret, config.client_secret);
        assert_eq!(
            reloaded.known_fingerprint("127.0.0.1:4000").as_deref(),
            Some("AB:CD")
        );
        assert_eq!(reloaded.last_server.as_deref(), Some("127.0.0.1:4000"));
    }

    #[test]
    fn client_fingerprint_does_not_panic_on_multibyte_secret() {
        // "a\u{e9}b" is 4 bytes ('a'=1, 'é'=2, 'b'=1) -- passes an even
        // *byte*-length check, but its char boundaries are at 0, 1, 3, 4, so
        // slicing the &str at byte offsets [0..2), [2..4) (as a naive hex
        // decoder does two bytes at a time) would panic on the second slice,
        // which starts inside the 2-byte 'é'. This must return a harmless
        // fingerprint instead of panicking.
        let config = Config {
            client_secret: "a\u{e9}b".to_string(),
            ..Config::default()
        };
        let fp = config.client_fingerprint();
        assert_eq!(
            fp.len(),
            64,
            "should fall back to an empty-byte hash, not panic"
        );
    }

    #[test]
    fn hex_decode_rejects_non_hex_without_panicking() {
        assert_eq!(hex_decode("gg"), None);
        assert_eq!(hex_decode("a"), None); // odd length
        assert_eq!(hex_decode(""), Some(Vec::new()));
        assert_eq!(hex_decode("ab"), Some(vec![0xab]));
        assert_eq!(hex_decode("AB"), Some(vec![0xab]));
    }

    #[test]
    fn forget_server_removes_it() {
        let mut config = Config::default();
        config.add_known_server("srv".into(), "10.0.0.1:4000".into(), "AA".into());
        assert!(config.forget_server("10.0.0.1:4000"));
        assert!(!config.forget_server("10.0.0.1:4000"));
        assert!(config.list_known_servers().is_empty());
    }

    #[test]
    fn save_restricts_file_to_owner() {
        // client.toml holds `client_secret`, a bearer credential once
        // paired -- `save()` must lock it down to the current user the same
        // way the server already does for its own comparably sensitive
        // files (server.key, server.toml, the audit log).
        let dir = TempDir::new("perms");
        let path = dir.path().join("client.toml");
        let mut config = Config::default();
        config.ensure_client_secret();
        config.save(&path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "config file should be owner-read/write only");
        }

        #[cfg(windows)]
        {
            // icacls is best-effort on this platform (see
            // `restrict_file_to_owner`); assert only that `save()` still
            // leaves a readable, correct file behind even after attempting
            // the ACL change.
            let reloaded = Config::load_or_default(&path);
            assert_eq!(reloaded.client_secret, config.client_secret);
        }
    }
}
