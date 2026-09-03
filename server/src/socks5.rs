use crate::server::{is_rate_limited, record_pairing_failure, RateLimitEntry};
use anyhow::Result;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tracing::{error, info, warn};

/// Per-IP lockout state for SOCKS5 authentication failures, shared across
/// all connections handled by one `Socks5Server::run` call. Reuses the same
/// policy (and the same `RateLimitEntry` type) as the pairing PIN limiter in
/// `server.rs`, since both defend against unlimited credential guessing.
type RateLimiter = Arc<Mutex<HashMap<IpAddr, RateLimitEntry>>>;

pub const SOCKS5_VERSION: u8 = 0x05;
pub const AUTH_NO_AUTH: u8 = 0x00;
pub const AUTH_USER_PASS: u8 = 0x02;
pub const CMD_CONNECT: u8 = 0x01;
pub const ATYP_IPV4: u8 = 0x01;
pub const ATYP_DOMAIN: u8 = 0x03;
pub const ATYP_IPV6: u8 = 0x04;

#[derive(Debug, Clone)]
pub struct Socks5Config {
    pub username: Option<String>,
    pub password: Option<String>,
    pub idle_timeout: Duration,
    /// When `false` (the default), clients MUST authenticate with
    /// username/password; an anonymous SOCKS5 proxy is an open relay to the
    /// whole LAN and must be opted into explicitly.
    pub allow_anonymous: bool,
}

impl Default for Socks5Config {
    fn default() -> Self {
        Self {
            username: None,
            password: None,
            idle_timeout: Duration::from_secs(300),
            allow_anonymous: false,
        }
    }
}

/// Constant-time byte comparison for credential checks. The length check is
/// a single integer comparison and so doesn't leak anything about *content*
/// (only how many bytes were sent, which the client already knows); once
/// lengths match, every byte is compared and XORed into one accumulator so
/// timing doesn't reveal how many leading bytes were correct.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub struct Socks5Server;

impl Socks5Server {
    pub async fn run(bind_address: &str, port: u16, config: Option<Socks5Config>) -> Result<()> {
        let addr = format!("{}:{}", bind_address, port);
        let listener = TcpListener::bind(&addr).await?;
        info!("SOCKS5 proxy server listening on {}", addr);

        let rate_limiter: RateLimiter = Arc::new(Mutex::new(HashMap::new()));

        loop {
            match listener.accept().await {
                Ok((stream, client_addr)) => {
                    info!("SOCKS5 connection request from {}", client_addr);
                    let cfg = config.clone();
                    let rate_limiter = rate_limiter.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            Self::handle_client(stream, cfg, client_addr.ip(), rate_limiter).await
                        {
                            error!("SOCKS5 error for {}: {}", client_addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept SOCKS5 connection: {}", e);
                }
            }
        }
    }

    pub async fn handle_client(
        mut stream: TcpStream,
        config: Option<Socks5Config>,
        peer_ip: IpAddr,
        rate_limiter: RateLimiter,
    ) -> Result<()> {
        let timeout_duration = config
            .as_ref()
            .map(|c| c.idle_timeout)
            .unwrap_or_else(|| Duration::from_secs(300));
        let target_addr = Self::negotiate(&mut stream, config, peer_ip, &rate_limiter).await?;

        info!("SOCKS5 connecting to target: {}", target_addr);

        match TcpStream::connect(&target_addr).await {
            Ok(mut target_stream) => {
                let reply = [SOCKS5_VERSION, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
                stream.write_all(&reply).await?;
                stream.flush().await?;

                let result = timeout(
                    timeout_duration,
                    tokio::io::copy_bidirectional(&mut stream, &mut target_stream),
                )
                .await;
                match result {
                    Ok(Err(e)) => Err(e.into()),
                    Err(_) => {
                        info!("SOCKS5 connection to {} timed out", target_addr);
                        Ok(())
                    }
                    Ok(Ok(_)) => Ok(()),
                }
            }
            Err(e) => {
                let reply = [SOCKS5_VERSION, 0x04, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
                let _ = stream.write_all(&reply).await;
                Err(e.into())
            }
        }
    }

    pub async fn negotiate<T: AsyncRead + AsyncWrite + Unpin>(
        stream: &mut T,
        config: Option<Socks5Config>,
        peer_ip: IpAddr,
        rate_limiter: &RateLimiter,
    ) -> Result<String> {
        let mut header = [0u8; 2];
        stream.read_exact(&mut header).await?;

        if header[0] != SOCKS5_VERSION {
            anyhow::bail!("Unsupported SOCKS version: {}", header[0]);
        }

        let num_methods = header[1] as usize;
        let mut methods = vec![0u8; num_methods];
        stream.read_exact(&mut methods).await?;

        // No config at all (used only by internal tests) behaves like an
        // explicit opt-in to anonymous access; a real deployment always
        // supplies a `Socks5Config`, and `allow_anonymous` there defaults to
        // `false` - open relays are opt-in, not the default.
        let allow_anonymous = config.as_ref().is_none_or(|c| c.allow_anonymous);
        let requires_auth = !allow_anonymous;

        if requires_auth && is_rate_limited(rate_limiter, peer_ip) {
            warn!(
                "Rejecting SOCKS5 auth from rate-limited peer {} (too many failed attempts)",
                peer_ip
            );
            let _ = stream.write_all(&[SOCKS5_VERSION, 0xFF]).await;
            anyhow::bail!("peer {} is rate-limited for SOCKS5 auth", peer_ip);
        }

        if requires_auth {
            let has_credentials = config
                .as_ref()
                .is_some_and(|c| c.username.is_some() && c.password.is_some());
            if !has_credentials {
                anyhow::bail!(
                    "SOCKS5 authentication required but no username/password is configured"
                );
            }

            if !methods.contains(&AUTH_USER_PASS) {
                stream.write_all(&[SOCKS5_VERSION, 0xFF]).await?;
                anyhow::bail!("Client does not support username/password authentication");
            }

            stream.write_all(&[SOCKS5_VERSION, AUTH_USER_PASS]).await?;
            stream.flush().await?;

            let mut auth_ver = [0u8; 1];
            stream.read_exact(&mut auth_ver).await?;
            if auth_ver[0] != 0x01 {
                stream.write_all(&[0x01, 0x01]).await?;
                anyhow::bail!("Unsupported auth version: {}", auth_ver[0]);
            }

            let mut ulen = [0u8; 1];
            stream.read_exact(&mut ulen).await?;
            let mut uname = vec![0u8; ulen[0] as usize];
            stream.read_exact(&mut uname).await?;

            let mut plen = [0u8; 1];
            stream.read_exact(&mut plen).await?;
            let mut pass = vec![0u8; plen[0] as usize];
            stream.read_exact(&mut pass).await?;

            let cfg = config.unwrap();
            let expected_user = cfg.username.as_deref().unwrap_or("");
            let expected_pass = cfg.password.as_deref().unwrap_or("");
            // Constant-time (content-wise) comparison, and combined with `&`
            // rather than `&&`, so neither which field is wrong nor how far
            // a guess got is observable via timing.
            let user_ok = constant_time_eq(&uname, expected_user.as_bytes());
            let pass_ok = constant_time_eq(&pass, expected_pass.as_bytes());
            if user_ok & pass_ok {
                stream.write_all(&[0x01, 0x00]).await?;
                stream.flush().await?;
            } else {
                record_pairing_failure(rate_limiter, peer_ip);
                stream.write_all(&[0x01, 0x01]).await?;
                stream.flush().await?;
                anyhow::bail!("Authentication failed");
            }
        } else {
            if !methods.contains(&AUTH_NO_AUTH) {
                stream.write_all(&[SOCKS5_VERSION, 0xFF]).await?;
                anyhow::bail!("Client does not support NO AUTH");
            }
            stream.write_all(&[SOCKS5_VERSION, AUTH_NO_AUTH]).await?;
            stream.flush().await?;
        }

        let mut req_header = [0u8; 4];
        stream.read_exact(&mut req_header).await?;

        let ver = req_header[0];
        let cmd = req_header[1];
        let atyp = req_header[3];

        if ver != SOCKS5_VERSION || cmd != CMD_CONNECT {
            stream
                .write_all(&[SOCKS5_VERSION, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            anyhow::bail!("Unsupported command or version");
        }

        let target_addr = match atyp {
            ATYP_IPV4 => {
                let mut ip_buf = [0u8; 4];
                stream.read_exact(&mut ip_buf).await?;
                let mut port_buf = [0u8; 2];
                stream.read_exact(&mut port_buf).await?;
                let port = u16::from_be_bytes(port_buf);
                format!("{}:{}", Ipv4Addr::from(ip_buf), port)
            }
            ATYP_DOMAIN => {
                let mut len_buf = [0u8; 1];
                stream.read_exact(&mut len_buf).await?;
                let domain_len = len_buf[0] as usize;
                let mut domain_buf = vec![0u8; domain_len];
                stream.read_exact(&mut domain_buf).await?;
                let mut port_buf = [0u8; 2];
                stream.read_exact(&mut port_buf).await?;
                let port = u16::from_be_bytes(port_buf);
                let domain = String::from_utf8(domain_buf)?;
                format!("{}:{}", domain, port)
            }
            ATYP_IPV6 => {
                let mut ip_buf = [0u8; 16];
                stream.read_exact(&mut ip_buf).await?;
                let mut port_buf = [0u8; 2];
                stream.read_exact(&mut port_buf).await?;
                let port = u16::from_be_bytes(port_buf);
                format!("[{}]:{}", Ipv6Addr::from(ip_buf), port)
            }
            _ => {
                stream
                    .write_all(&[SOCKS5_VERSION, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await?;
                anyhow::bail!("Unsupported address type: {}", atyp);
            }
        };

        Ok(target_addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn test_ip() -> IpAddr {
        "127.0.0.1".parse().unwrap()
    }

    fn test_limiter() -> RateLimiter {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[tokio::test]
    async fn test_negotiate_no_auth() {
        let (mut client, mut server) = duplex(1024);
        let limiter = test_limiter();

        let handle = tokio::spawn(async move {
            Socks5Server::negotiate(&mut server, None, test_ip(), &limiter).await
        });

        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();

        let mut resp = [0u8; 2];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0x00]);

        client
            .write_all(&[0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1, 0, 80])
            .await
            .unwrap();

        let target = handle.await.unwrap().unwrap();
        assert_eq!(target, "127.0.0.1:80");
    }

    #[tokio::test]
    async fn test_negotiate_auth_success() {
        let (mut client, mut server) = duplex(1024);
        let config = Socks5Config {
            username: Some("user".to_string()),
            password: Some("pass".to_string()),
            idle_timeout: Duration::from_secs(300),
            allow_anonymous: false,
        };

        let limiter = test_limiter();
        let handle = tokio::spawn(async move {
            Socks5Server::negotiate(&mut server, Some(config), test_ip(), &limiter).await
        });

        client.write_all(&[0x05, 0x02, 0x00, 0x02]).await.unwrap();

        let mut resp = [0u8; 2];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0x02]);

        let mut auth_req = vec![0x01, 4];
        auth_req.extend_from_slice(b"user");
        auth_req.push(4);
        auth_req.extend_from_slice(b"pass");
        client.write_all(&auth_req).await.unwrap();

        let mut auth_resp = [0u8; 2];
        client.read_exact(&mut auth_resp).await.unwrap();
        assert_eq!(auth_resp, [0x01, 0x00]);

        client
            .write_all(&[0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1, 0, 80])
            .await
            .unwrap();

        let target = handle.await.unwrap().unwrap();
        assert_eq!(target, "127.0.0.1:80");
    }

    #[tokio::test]
    async fn test_negotiate_rejects_anonymous_when_not_allowed_and_unconfigured() {
        let (mut client, mut server) = duplex(1024);
        let config = Socks5Config {
            username: None,
            password: None,
            idle_timeout: Duration::from_secs(300),
            allow_anonymous: false,
        };

        let limiter = test_limiter();
        let handle = tokio::spawn(async move {
            Socks5Server::negotiate(&mut server, Some(config), test_ip(), &limiter).await
        });

        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();

        let result = handle.await.unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_constant_time_eq() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secre1"));
        assert!(!constant_time_eq(b"secret", b"short"));
        assert!(!constant_time_eq(b"", b"nonempty"));
        assert!(constant_time_eq(b"", b""));
    }

    #[tokio::test]
    async fn test_negotiate_rejects_rate_limited_peer() {
        let limiter = test_limiter();
        // Simulate 5 prior failures from this IP, same as the pairing limiter.
        for _ in 0..5 {
            record_pairing_failure(&limiter, test_ip());
        }
        assert!(is_rate_limited(&limiter, test_ip()));

        let (mut client, mut server) = duplex(1024);
        let config = Socks5Config {
            username: Some("user".to_string()),
            password: Some("pass".to_string()),
            idle_timeout: Duration::from_secs(300),
            allow_anonymous: false,
        };
        let handle = tokio::spawn(async move {
            Socks5Server::negotiate(&mut server, Some(config), test_ip(), &limiter).await
        });

        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();

        let result = handle.await.unwrap();
        assert!(
            result.is_err(),
            "a rate-limited peer must be rejected before it can even attempt credentials"
        );
    }
}
