//! TLS certificate pinning for the client's outbound connections.
//!
//! The server presents a self-signed certificate. There is no CA to validate
//! it against, so trust is established the same way SSH does it: the client
//! remembers the certificate's fingerprint the first time it connects to a
//! given address (trust-on-first-use) and refuses to proceed if a later
//! connection to that same address presents a different one, unless the
//! caller explicitly overrides it (`--trust-new-cert` / `--fingerprint`).
//!
//! Signature verification is still performed for real (not skipped): this
//! verifier only replaces the *chain-of-trust* check (there is no chain to a
//! root CA) with a fingerprint check, and delegates the cryptographic
//! signature verification to rustls' own webpki-backed helpers.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

/// Render the SHA-256 fingerprint of a DER certificate as uppercase
/// colon-separated hex pairs (`AB:CD:...`), matching the format the server
/// prints and the format used in `known_servers` / `--fingerprint`.
pub fn fingerprint_der(cert: &CertificateDer<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cert.as_ref());
    let hash = hasher.finalize();
    hash.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// A [`ServerCertVerifier`] that pins a server certificate by its SHA-256
/// fingerprint instead of validating a certificate chain.
///
/// * `required = Some(fp)`: the presented certificate must match `fp`
///   (case-insensitively) or the handshake is rejected.
/// * `required = None`: any certificate is accepted (first connection to an
///   address, or `--trust-new-cert`); the caller reads the fingerprint back
///   out of `observed` afterwards and pins it.
///
/// Either way, the fingerprint of whatever certificate was actually
/// presented is recorded into `observed` so the caller can decide what to
/// persist.
#[derive(Debug)]
pub struct PinnedCertVerifier {
    required: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
    algs: WebPkiSupportedAlgorithms,
}

impl PinnedCertVerifier {
    pub fn new(required: Option<String>, observed: Arc<Mutex<Option<String>>>) -> Self {
        Self {
            required: required.map(|s| s.to_uppercase()),
            observed,
            algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let fp = fingerprint_der(end_entity);
        *self.observed.lock().unwrap() = Some(fp.clone());

        match &self.required {
            Some(expected) if expected.eq_ignore_ascii_case(&fp) => {
                Ok(ServerCertVerified::assertion())
            }
            Some(expected) => Err(Error::General(format!(
                "server certificate fingerprint mismatch: expected {expected}, got {fp}. \
                 Pass --trust-new-cert to trust the new certificate, or verify --fingerprint."
            ))),
            None => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::UnixTime;

    fn self_signed_der() -> CertificateDer<'static> {
        let cert = generate_simple_self_signed(vec!["kvm-server".to_string()]).unwrap();
        CertificateDer::from(cert.serialize_der().unwrap())
    }

    #[test]
    fn first_use_accepts_and_records_fingerprint() {
        let der = self_signed_der();
        let observed = Arc::new(Mutex::new(None));
        let verifier = PinnedCertVerifier::new(None, observed.clone());
        let server_name = ServerName::try_from("kvm-server").unwrap();

        let result = verifier.verify_server_cert(&der, &[], &server_name, &[], UnixTime::now());
        assert!(result.is_ok());
        let fp = observed.lock().unwrap().clone().unwrap();
        assert_eq!(fp, fingerprint_der(&der));
        assert!(fp.contains(':'));
    }

    #[test]
    fn matching_fingerprint_is_accepted() {
        let der = self_signed_der();
        let expected = fingerprint_der(&der);
        let observed = Arc::new(Mutex::new(None));
        let verifier = PinnedCertVerifier::new(Some(expected.to_lowercase()), observed);
        let server_name = ServerName::try_from("kvm-server").unwrap();

        let result = verifier.verify_server_cert(&der, &[], &server_name, &[], UnixTime::now());
        assert!(result.is_ok(), "case-insensitive match should be accepted");
    }

    #[test]
    fn mismatched_fingerprint_is_rejected() {
        let der = self_signed_der();
        let observed = Arc::new(Mutex::new(None));
        let verifier = PinnedCertVerifier::new(
            Some("00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00".to_string()),
            observed,
        );
        let server_name = ServerName::try_from("kvm-server").unwrap();

        let result = verifier.verify_server_cert(&der, &[], &server_name, &[], UnixTime::now());
        assert!(result.is_err());
    }
}
