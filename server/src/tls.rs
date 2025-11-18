use anyhow::Result;
use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::ServerConfig;
use rustls_pemfile::{certs, pkcs8_private_keys};
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

    let CertifiedKey { cert, key_pair } = generate_simple_self_signed(subject_alt_names)?;

    fs::write(cert_path, cert.pem())?;
    fs::write(key_path, key_pair.serialize_pem())?;

    info!("TLS certificates generated at {:?}", cert_path.parent());

    Ok(())
}

fn load_certs(path: &PathBuf) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
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

    Ok(rustls::pki_types::PrivateKeyDer::Pkcs8(keys[0].clone()))
}

pub fn get_certificate_fingerprint() -> Result<String> {
    use sha2::{Digest, Sha256};

    let cert_dir = get_cert_dir()?;
    let cert_path = cert_dir.join("server.crt");

    let cert_pem = fs::read(&cert_path)?;
    let mut hasher = Sha256::new();
    hasher.update(&cert_pem);
    let hash = hasher.finalize();

    Ok(format!("{:x}", hash))
}
