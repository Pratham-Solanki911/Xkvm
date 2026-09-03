use crate::addr::format_host_port;
use anyhow::Result;
use kvm_common::MDNS_SERVICE_TYPE;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// `host:port`, IPv6 hosts bracketed -- ready to hand to `Session::connect`.
    pub address: String,
}

/// Browse for KVM-RS servers for `timeout`, collecting every distinct
/// service seen (deduped by mDNS fullname) rather than stopping at the
/// first. Results are sorted by name; when a server advertises more than
/// one address, an IPv4 address is preferred over IPv6.
pub async fn discover_servers(timeout: Duration) -> Result<Vec<DiscoveredServer>> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(MDNS_SERVICE_TYPE)?;

    let mut found: HashMap<String, DiscoveredServer> = HashMap::new();
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            event = tokio::task::spawn_blocking({
                let receiver = receiver.clone();
                move || receiver.recv_timeout(Duration::from_millis(100))
            }) => {
                match event {
                    Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                        let fullname = info.get_fullname().to_string();
                        let port = info.get_port();

                        let v4: Option<IpAddr> = info
                            .get_addresses_v4()
                            .into_iter()
                            .next()
                            .map(|a| IpAddr::V4(*a));
                        let any = info.get_addresses().iter().next().copied();

                        if let Some(addr) = v4.or(any) {
                            let host = addr.to_string();
                            let name = info
                                .get_hostname()
                                .trim_end_matches('.')
                                .to_string();
                            let name = if name.is_empty() { fullname.clone() } else { name };
                            found.insert(
                                fullname,
                                DiscoveredServer {
                                    name,
                                    address: format_host_port(&host, port),
                                    host,
                                    port,
                                },
                            );
                        }
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => debug!("mDNS receive error: {}", e),
                    Err(e) => debug!("mDNS task error: {}", e),
                }
            }
            _ = &mut deadline => break,
        }
    }

    let _ = daemon.shutdown();

    let mut servers: Vec<_> = found.into_values().collect();
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(servers)
}
