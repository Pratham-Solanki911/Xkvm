use anyhow::Result;
use kvm_common::MDNS_SERVICE_TYPE;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::time::Duration;
use tracing::{debug, info};

#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    pub name: String,
    pub address: String,
    pub port: u16,
}

pub async fn discover_servers() -> Result<Vec<DiscoveredServer>> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(MDNS_SERVICE_TYPE)?;

    let mut servers = Vec::new();
    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            event = tokio::task::spawn_blocking({
                let receiver = receiver.clone();
                move || receiver.recv_timeout(Duration::from_millis(100))
            }) => {
                match event {
                    Ok(Ok(event)) => {
                        match event {
                            ServiceEvent::ServiceResolved(info) => {
                                info!("Discovered server: {}", info.get_fullname());

                                let addresses = info.get_addresses();
                                if let Some(addr) = addresses.iter().next() {
                                    let server = DiscoveredServer {
                                        name: info.get_fullname().to_string(),
                                        address: format!("{}:{}", addr, info.get_port()),
                                        port: info.get_port(),
                                    };
                                    servers.push(server);
                                }
                            }
                            ServiceEvent::SearchStarted(_) => {
                                debug!("mDNS search started");
                            }
                            _ => {}
                        }
                    }
                    Ok(Err(e)) => {
                        debug!("mDNS receive error: {}", e);
                    }
                    Err(e) => {
                        debug!("Task error: {}", e);
                    }
                }
            }
            _ = &mut timeout => {
                info!("Discovery timeout");
                break;
            }
        }
    }

    Ok(servers)
}
