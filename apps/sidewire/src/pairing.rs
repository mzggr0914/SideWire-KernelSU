use super::*;
use sidewire_protocol::{
    PAIRING_PORT, PairBanner, PairCommit, PairComplete, PairReply, PairStart, decode, encode,
    noise_initiator, pairing_psk, read_noise_record, read_packet, security_prologue,
    write_noise_record, write_packet,
};
use spake2::{Ed25519Group, Identity, Password, Spake2};

fn pair_endpoint(target: &str) -> String {
    if target
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
    {
        target.to_owned()
    } else {
        format!("{target}:{PAIRING_PORT}")
    }
}

fn read_pin() -> Result<String> {
    print!("Pairing PIN: ");
    io::stdout().flush()?;
    let mut pin = String::new();
    std::io::stdin().read_line(&mut pin)?;
    let pin = pin.trim().to_owned();
    if pin.len() != 6 || !pin.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("pairing PIN must be exactly 6 digits");
    }
    Ok(pin)
}
pub(super) async fn run_pair(target: Option<String>, discover: bool) -> Result<()> {
    if discover && target.is_some() {
        bail!("use either a target or --discover, not both");
    }
    let target = if discover {
        let found =
            crate::discovery::discover_all(tokio::time::Duration::from_millis(1200)).await?;
        let compatible: Vec<_> = found
            .into_iter()
            .filter(|d| d.protocol_version == sidewire_protocol::VERSION)
            .collect();
        match compatible.len() {
            0 => bail!("no pairable SideWire device discovered"),
            1 => {
                let addr: std::net::SocketAddr = compatible[0].endpoint.parse()?;
                std::net::SocketAddr::new(addr.ip(), PAIRING_PORT).to_string()
            }
            _ => bail!("multiple devices discovered; specify the phone IP explicitly"),
        }
    } else {
        pair_endpoint(
            target
                .as_deref()
                .context("provide a phone IP or use --discover")?,
        )
    };
    let endpoint = target;
    let mut stream = TcpStream::connect(&endpoint)
        .await
        .with_context(|| format!("connect SideWire pairing endpoint {endpoint}"))?;
    stream.set_nodelay(true)?;
    let banner: PairBanner = read_packet(&mut stream).await?;
    if banner.protocol_version != sidewire_protocol::VERSION {
        bail!(
            "pairing protocol mismatch: device {}, host {}",
            banner.protocol_version,
            sidewire_protocol::VERSION
        );
    }
    if !banner.pairing_available {
        bail!("pairing is not enabled on {}", banner.name);
    }
    println!("Device: {} [{}]", banner.name, banner.device_id.short());
    let pin = read_pin()?;
    let host_id = crate::trust::host_id()?;
    let host_name = crate::trust::host_name();
    let host_id_text = host_id.to_hex();
    let device_id_text = banner.device_id.to_hex();
    let id_a = Identity::new(host_id_text.as_bytes());
    let id_b = Identity::new(device_id_text.as_bytes());
    let (state, message) = Spake2::<Ed25519Group>::start_a(&Password::new(pin), &id_a, &id_b);
    write_packet(
        &mut stream,
        &PairStart {
            host_id,
            host_name: host_name.clone(),
            spake_message: message,
        },
    )
    .await?;
    let reply: PairReply = read_packet(&mut stream).await?;
    let shared = state
        .finish(&reply.spake_message)
        .map_err(|_| anyhow::anyhow!("pairing key exchange failed"))?;
    let pairing_key = pairing_psk(&shared, host_id, banner.device_id);
    let prologue = security_prologue(host_id, banner.device_id);
    let noise = noise_initiator(&mut stream, &pairing_key, &prologue)
        .await
        .context("pairing authentication failed; check the PIN")?;
    let shared_secret = rand::random::<[u8; 32]>();
    let commit = encode(&PairCommit {
        host_id,
        host_name,
        shared_secret,
    })?;
    write_noise_record(&mut stream, &noise, &commit).await?;
    let complete_bytes = read_noise_record(&mut stream, &noise).await?;
    let complete: PairComplete = decode(&complete_bytes)?;
    if complete.device_id != banner.device_id {
        bail!("paired device identity changed during handshake");
    }
    crate::trust::save_device(
        complete.device_id,
        complete.device_name.clone(),
        shared_secret,
    )?;
    println!(
        "Paired with {} [{}]. Future secure connections are automatic.",
        complete.device_name,
        complete.device_id.short()
    );
    Ok(())
}

pub(super) fn run_paired() -> Result<()> {
    let devices = crate::trust::list_devices()?;
    if devices.is_empty() {
        println!("No paired devices.");
        return Ok(());
    }
    println!("ID\tNAME");
    for device in devices {
        println!("{}\t{}", device.id.short(), device.name);
    }
    Ok(())
}
pub(super) fn run_unpair(selector: String) -> Result<()> {
    let removed = crate::trust::remove_device(&selector)?;
    println!(
        "Removed pairing for {} [{}]",
        removed.name,
        removed.id.short()
    );
    Ok(())
}
