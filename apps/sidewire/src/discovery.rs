use super::*;
use sidewire_protocol::{DISCOVERY_PORT, DISCOVERY_REQUEST, DiscoveryReply};
use tokio::{net::UdpSocket, time::Duration};

#[derive(Debug, Clone)]
pub(super) struct DiscoveredDevice {
    pub name: String,
    pub endpoint: String,
    pub protocol_version: u16,
}

pub(super) async fn discover_one(timeout: Duration) -> Result<Option<DiscoveredDevice>> {
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

    let deadline = tokio::time::Instant::now() + timeout;
    let mut buffer = [0u8; 2048];
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        let remaining = deadline - now;
        let received = match tokio::time::timeout(remaining, socket.recv_from(&mut buffer)).await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => return Err(error).context("receive SideWire discovery reply"),
            Err(_) => return Ok(None),
        };
        let (size, peer) = received;
        let Ok(reply) = sidewire_protocol::decode::<DiscoveryReply>(&buffer[..size]) else {
            continue;
        };
        return Ok(Some(DiscoveredDevice {
            name: reply.name,
            endpoint: format!("{}:{}", peer.ip(), reply.port),
            protocol_version: reply.protocol_version,
        }));
    }
}

pub(super) async fn run_discover(timeout_ms: u64) -> Result<()> {
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let Some(device) = discover_one(timeout).await? else {
        bail!("no inbound SideWire device discovered");
    };
    println!("NAME\tENDPOINT\tPROTOCOL");
    println!(
        "{}\t{}\t{}",
        device.name, device.endpoint, device.protocol_version
    );
    if device.protocol_version != sidewire_protocol::VERSION {
        bail!(
            "protocol mismatch: CLI={} device={}",
            sidewire_protocol::VERSION,
            device.protocol_version
        );
    }
    Ok(())
}
