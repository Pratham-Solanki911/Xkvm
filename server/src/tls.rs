use anyhow::Result;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::CertificateDer;
use rustls::ServerConfig;
use rustls_pemfile::{certs, pkcs8_private_keys};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

pub fn setup_tls_server() -> Result<Arc<ServerConfig>> {
    let cert_dir = get_cert_dir()?;
    fs::create_dir_all(&cert_dir)?;

    let cert_path = cert_dir.join("server.crt");
    let key_path = cert_dir.join("server.key");

    // Generate or load certificates
    if !cert_path.exists() || !key_path.exists() {
        info!("Generating new TLS certificates...");
        generate_certificates(&cert_path, &key_path)?;
    }

    // Load certificates
    let certs = load_certs(&cert_path)?;
    let key = load_private_key(&key_path)?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(Arc::new(config))
}

fn get_cert_dir() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;
    Ok(config_dir.join("kvm-rs"))
}

fn generate_certificates(cert_path: &PathBuf, key_path: &PathBuf) -> Result<()> {
    let subject_alt_names = vec!["localhost".to_string(), "kvm-server".to_string()];

    let certified_key = generate_simple_self_signed(subject_alt_names)?;

    fs::write(cert_path, certified_key.serialize_pem()?)?;
    write_private_key(key_path, &certified_key.serialize_private_key_pem())?;

    info!("TLS certificates generated at {:?}", cert_path.parent());

    Ok(())
}

/// Writes the private key PEM with owner-only permissions on unix.
#[cfg(unix)]
fn write_private_key(path: &PathBuf, pem: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(pem.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_key(path: &PathBuf, pem: &str) -> Result<()> {
    fs::write(path, pem)?;
    restrict_file_to_owner(path)?;
    Ok(())
}

/// Restricts `path` to the current user only, best-effort on the caller's
/// behalf but reported as an error so callers can decide whether it's fatal.
/// Unix: `chmod 0600`. Windows: replace inherited ACLs with a single
/// full-control entry for the current user via `icacls` (no extra crate
/// needed). Used for the TLS private key and for config/log files that hold
/// plaintext secrets (pairing PIN, SOCKS5 password, paired-device
/// fingerprints).
pub fn restrict_file_to_owner(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
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

fn load_certs(path: &PathBuf) -> Result<Vec<CertificateDer<'static>>> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let certs = certs(&mut reader).collect::<Result<Vec<_>, _>>()?;
    Ok(certs)
}

fn load_private_key(path: &PathBuf) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let keys = pkcs8_private_keys(&mut reader).collect::<Result<Vec<_>, _>>()?;

    if keys.is_empty() {
        anyhow::bail!("No private key found in {}", path.display());
    }

    Ok(rustls::pki_types::PrivateKeyDer::Pkcs8(keys[0].clone_key()))
}

/// SHA-256 fingerprint of a DER-encoded certificate, formatted as uppercase
/// colon-separated hex pairs (`AB:CD:...`), per the trust model.
pub fn fingerprint_der(cert: &CertificateDer) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cert.as_ref());
    let hash = hasher.finalize();
    hash.iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(":")
}

/// Fingerprint of the server's own certificate, computed over the DER bytes
/// (not the PEM file contents).
pub fn get_certificate_fingerprint() -> Result<String> {
    let cert_dir = get_cert_dir()?;
    let cert_path = cert_dir.join("server.crt");
    let certs = load_certs(&cert_path)?;
    let cert = certs
        .first()
        .ok_or_else(|| anyhow::anyhow!("no certificate found in {}", cert_path.display()))?;
    Ok(fingerprint_der(cert))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_der_is_deterministic_and_formatted() {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let der = CertificateDer::from(cert.serialize_der().unwrap());

        let fp1 = fingerprint_der(&der);
        let fp2 = fingerprint_der(&der);
        assert_eq!(fp1, fp2);
        // Uppercase colon-separated hex pairs, 32 bytes -> 32*2 hex chars + 31 colons.
        assert_eq!(fp1.len(), 32 * 2 + 31);
        assert!(fp1.chars().all(|c| c.is_ascii_hexdigit() || c == ':'));
        assert_eq!(fp1, fp1.to_uppercase());
    }

    #[test]
    fn test_fingerprint_der_differs_for_different_certs() {
        let cert_a = rcgen::generate_simple_self_signed(vec!["a.local".to_string()]).unwrap();
        let cert_b = rcgen::generate_simple_self_signed(vec!["b.local".to_string()]).unwrap();
        let der_a = CertificateDer::from(cert_a.serialize_der().unwrap());
        let der_b = CertificateDer::from(cert_b.serialize_der().unwrap());
        assert_ne!(fingerprint_der(&der_a), fingerprint_der(&der_b));
    }

    #[test]
    fn test_restrict_file_to_owner_leaves_content_intact() {
        let path = std::env::temp_dir().join(format!("kvm_restrict_test_{}", uuid::Uuid::new_v4()));
        fs::write(&path, b"secret").unwrap();

        restrict_file_to_owner(&path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"secret");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        fs::remove_file(&path).ok();
    }
}
