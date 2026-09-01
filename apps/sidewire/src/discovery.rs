use super::*;
use sidewire_protocol::{DISCOVERY_PORT, DISCOVERY_REQUEST, DeviceId, DiscoveryReply};
use std::collections::HashMap;
use tokio::{net::UdpSocket, time::Duration};

#[derive(Debug, Clone)]
pub(super) struct DiscoveredDevice {
    pub device_id: DeviceId,
    pub name: String,
    pub endpoint: String,
    pub protocol_version: u16,
}

pub(super) async fn discover_all(timeout: Duration) -> Result<Vec<DiscoveredDevice>> {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("bind SideWire discovery socket")?;
    socket
        .set_broadcast(true)
        .context("enable SideWire discovery broadcast")?;
    socket
        .send_to(DISCOVERY_REQUEST, ("255.255.255.255", DISCOVERY_PORT))
        .await
        .context("send SideWire discovery broadcast")?;
    let _ = socket
        .send_to(DISCOVERY_REQUEST, ("127.0.0.1", DISCOVERY_PORT))
        .await;

    let deadline = tokio::time::Instant::now() + timeout;
    let mut buffer = [0u8; 2048];
    let mut found = HashMap::new();
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        let received = match tokio::time::timeout(remaining, socket.recv_from(&mut buffer)).await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => return Err(error).context("receive SideWire discovery reply"),
            Err(_) => break,
        };
        let (size, peer) = received;
        let Ok(reply) = sidewire_protocol::decode::<DiscoveryReply>(&buffer[..size]) else {
            continue;
        };
        found.insert(
            reply.device_id,
            DiscoveredDevice {
                device_id: reply.device_id,
                name: reply.name,
                endpoint: format!("{}:{}", peer.ip(), reply.port),
                protocol_version: reply.protocol_version,
            },
        );
    }
    let mut devices: Vec<_> = found.into_values().collect();
    devices.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.device_id.0.cmp(&right.device_id.0))
    });
    Ok(devices)
}

pub(super) async fn run_discover(timeout_ms: u64) -> Result<()> {
    let devices = discover_all(Duration::from_millis(timeout_ms.max(1))).await?;
    if devices.is_empty() {
        bail!("no inbound SideWire devices discovered");
    }
    println!("ID\tNAME\tENDPOINT\tPROTOCOL\tSTATUS");
    for device in devices {
        let compatible = device.protocol_version == sidewire_protocol::VERSION;
        println!(
            "{}\t{}\t{}\t{}\t{}",
            device.device_id.short(),
            device.name,
            device.endpoint,
            device.protocol_version,
            if compatible { "ready" } else { "mismatch" }
        );
    }
    Ok(())
}
