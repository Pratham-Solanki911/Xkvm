use anyhow::Result;
use kvm_common::{Caps, MDNS_SERVICE_TYPE};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
use tracing::{info, warn};

pub struct MdnsService {
    daemon: ServiceDaemon,
}

pub async fn start_mdns_server(port: u16, server_name: &str) -> Result<MdnsService> {
    let daemon = ServiceDaemon::new()?;

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "kvm-server".to_string());

    // The mDNS instance name is derived from the configured server name (not
    // the raw OS hostname) so it matches what clients will see as
    // `HelloAck.server_name`.
    let instance_name = format!("{}._kvm-rs._tcp.local.", server_name);

    let caps = Caps::default();
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), server_name.to_string());
    properties.insert("port".to_string(), port.to_string());
    properties.insert("clipboard".to_string(), caps.clipboard.to_string());
    properties.insert("file_transfer".to_string(), caps.file_transfer.to_string());
    properties.insert(
        "internet_share".to_string(),
        caps.internet_share.to_string(),
    );

    let service_info = ServiceInfo::new(
        MDNS_SERVICE_TYPE,
        &instance_name,
        &hostname,
        (),
        port,
        Some(properties),
    )?;

    daemon.register(service_info)?;

    info!(
        "mDNS service registered: {} on port {}",
        instance_name, port
    );

    Ok(MdnsService { daemon })
}

impl Drop for MdnsService {
    fn drop(&mut self) {
        if let Err(e) = self.daemon.shutdown() {
            warn!("Failed to shut down mDNS daemon: {:?}", e);
        }
    }
}
