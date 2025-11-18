use anyhow::Result;
use kvm_common::{Caps, MDNS_SERVICE_TYPE};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
use tracing::{error, info};

pub struct MdnsService {
    _daemon: ServiceDaemon,
}

pub async fn start_mdns_server(port: u16) -> Result<MdnsService> {
    let daemon = ServiceDaemon::new()?;

    let hostname = hostname::get()?
        .into_string()
        .unwrap_or_else(|_| "kvm-server".to_string());

    let instance_name = format!("{}._kvm-rs._tcp.local.", hostname);

    let caps = Caps::default();
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), hostname.clone());
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

    Ok(MdnsService { _daemon: daemon })
}
