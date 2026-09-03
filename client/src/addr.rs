//! Address parsing helpers that understand `host:port`, bare hosts, and both
//! bracketed (`[::1]:4000`) and bare IPv6 literals.

use anyhow::{bail, Result};

/// Split `s` into a `(host, port)` pair, using `default_port` when `s` carries
/// no port. Accepts `[v6]:port`, `[v6]` (no port), `host:port`, a bare
/// hostname/IPv4 address, or a bare (unbracketed) IPv6 literal.
pub fn parse_host_port(s: &str, default_port: u16) -> Result<(String, u16)> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty address");
    }

    if let Some(rest) = s.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| anyhow::anyhow!("invalid address '{}': missing closing ']'", s))?;
        let host = &rest[..end];
        if host.is_empty() {
            bail!("invalid address '{}': empty host", s);
        }
        let after = &rest[end + 1..];
        let port = if let Some(p) = after.strip_prefix(':') {
            p.parse::<u16>()
                .map_err(|_| anyhow::anyhow!("invalid port in '{}': '{}'", s, p))?
        } else if after.is_empty() {
            default_port
        } else {
            bail!("invalid address '{}': unexpected trailing characters", s);
        };
        return Ok((host.to_string(), port));
    }

    match s.matches(':').count() {
        0 => Ok((s.to_string(), default_port)),
        1 => {
            let (host, port_str) = s.rsplit_once(':').unwrap();
            if host.is_empty() {
                bail!("invalid address '{}': empty host", s);
            }
            let port: u16 = port_str
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid port in '{}': '{}'", s, port_str))?;
            Ok((host.to_string(), port))
        }
        // Multiple colons with no brackets: a bare IPv6 literal. Treat the
        // whole string as the host and use the default port (there is no
        // unambiguous way to split a trailing port off a bare v6 address).
        _ => Ok((s.to_string(), default_port)),
    }
}

/// Format `(host, port)` back into a connectable string, bracketing IPv6
/// hosts so the result round-trips through [`parse_host_port`].
pub fn format_host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_uses_default_port() {
        assert_eq!(
            parse_host_port("example.com", 4000).unwrap(),
            ("example.com".to_string(), 4000)
        );
    }

    #[test]
    fn host_colon_port() {
        assert_eq!(
            parse_host_port("example.com:4100", 4000).unwrap(),
            ("example.com".to_string(), 4100)
        );
    }

    #[test]
    fn bracketed_v6_with_port() {
        assert_eq!(
            parse_host_port("[::1]:4100", 4000).unwrap(),
            ("::1".to_string(), 4100)
        );
    }

    #[test]
    fn bracketed_v6_without_port() {
        assert_eq!(
            parse_host_port("[fe80::1]", 4000).unwrap(),
            ("fe80::1".to_string(), 4000)
        );
    }

    #[test]
    fn bare_v6_uses_default_port() {
        assert_eq!(
            parse_host_port("fe80::1:2:3", 4000).unwrap(),
            ("fe80::1:2:3".to_string(), 4000)
        );
    }

    #[test]
    fn empty_address_is_error() {
        assert!(parse_host_port("", 4000).is_err());
    }

    #[test]
    fn empty_host_before_colon_is_error() {
        assert!(parse_host_port(":4000", 4000).is_err());
    }

    #[test]
    fn invalid_port_is_error() {
        assert!(parse_host_port("example.com:notaport", 4000).is_err());
    }

    #[test]
    fn missing_closing_bracket_is_error() {
        assert!(parse_host_port("[::1", 4000).is_err());
    }

    #[test]
    fn format_round_trips_v4_and_v6() {
        assert_eq!(format_host_port("example.com", 4000), "example.com:4000");
        assert_eq!(format_host_port("::1", 4000), "[::1]:4000");
        let (h, p) = parse_host_port(&format_host_port("::1", 4000), 0).unwrap();
        assert_eq!((h.as_str(), p), ("::1", 4000));
    }
}
