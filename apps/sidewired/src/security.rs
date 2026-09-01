use anyhow::{Context, Result, bail};
use sidewire_protocol::{
    DeviceId, PairBanner, PairCommit, PairComplete, PairReply, PairStart, SecurityBanner,
    SecurityClientHello, SecurityDecision, SecurityMode, SharedNoise, decode, encode, key_from_hex,
    key_to_hex, noise_initiator, noise_responder, pairing_psk, read_noise_record, read_packet,
    security_prologue, write_noise_record, write_packet,
};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::{Duration, sleep, timeout},
};

pub(super) struct ConnectionSecurity {
    pub peer_id: DeviceId,
    pub noise: Option<SharedNoise>,
    pub shared_secret: Option<[u8; 32]>,
}

fn paired_path(pairs_dir: &str, host_id: DeviceId) -> PathBuf {
    Path::new(pairs_dir).join(format!("{}.pair", host_id.to_hex()))
}

fn paired_host_secret(pairs_dir: Option<&str>, host_id: DeviceId) -> Result<Option<[u8; 32]>> {
    let Some(dir) = pairs_dir else {
        return Ok(None);
    };
    let path = paired_path(dir, host_id);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read paired host {}", path.display()))?;
    let secret = text.lines().find_map(|line| line.strip_prefix("secret="));
    secret.map(key_from_hex).transpose()
}
fn store_paired_host(
    pairs_dir: &str,
    host_id: DeviceId,
    host_name: &str,
    secret: [u8; 32],
) -> Result<()> {
    fs::create_dir_all(pairs_dir)?;
    let safe_name: String = host_name
        .chars()
        .filter(|ch| !matches!(ch, '\r' | '\n'))
        .collect();
    let path = paired_path(pairs_dir, host_id);
    fs::write(
        &path,
        format!("name={safe_name}\nsecret={}\n", key_to_hex(&secret)),
    )
    .with_context(|| format!("write paired host {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

async fn reject(stream: &mut TcpStream, message: String) -> Result<ConnectionSecurity> {
    write_packet(
        stream,
        &SecurityDecision {
            accepted: false,
            message: message.clone(),
        },
    )
    .await?;
    bail!(message)
}

pub(super) async fn accept_connection(
    stream: &mut TcpStream,
    device_id: DeviceId,
    mode: SecurityMode,
    pairs_dir: Option<&str>,
) -> Result<ConnectionSecurity> {
    write_packet(
        stream,
        &SecurityBanner {
            node_id: device_id,
            security: mode,
            protocol_version: sidewire_protocol::VERSION,
        },
    )
    .await?;
    let hello: SecurityClientHello = read_packet(stream).await?;
    if hello.protocol_version != sidewire_protocol::VERSION {
        return reject(
            stream,
            format!(
                "protocol mismatch: host {}, device {}",
                hello.protocol_version,
                sidewire_protocol::VERSION
            ),
        )
        .await;
    }
    if hello.security != mode {
        return reject(
            stream,
            format!(
                "security mode mismatch: host {}, device {}",
                hello.security.as_str(),
                mode.as_str()
            ),
        )
        .await;
    }
    let secret = match mode {
        SecurityMode::Secure => match paired_host_secret(pairs_dir, hello.node_id)? {
            Some(secret) => Some(secret),
            None => {
                return reject(
                    stream,
                    format!("host {} is not paired", hello.node_id.short()),
                )
                .await;
            }
        },
        SecurityMode::Insecure => None,
    };
    write_packet(
        stream,
        &SecurityDecision {
            accepted: true,
            message: "ok".into(),
        },
    )
    .await?;
    let noise = if let Some(secret) = secret {
        let prologue = security_prologue(hello.node_id, device_id);
        Some(noise_responder(stream, &secret, &prologue).await?)
    } else {
        None
    };
    Ok(ConnectionSecurity {
        peer_id: hello.node_id,
        noise,
        shared_secret: secret,
    })
}

pub(super) async fn connect_connection(
    stream: &mut TcpStream,
    device_id: DeviceId,
    mode: SecurityMode,
    pairs_dir: Option<&str>,
) -> Result<ConnectionSecurity> {
    let banner: SecurityBanner = read_packet(stream).await?;
    if banner.protocol_version != sidewire_protocol::VERSION {
        bail!(
            "protocol mismatch: host {}, device {}",
            banner.protocol_version,
            sidewire_protocol::VERSION
        );
    }
    if banner.security != mode {
        bail!(
            "security mode mismatch: host {}, device {}; both sides must explicitly use the same mode",
            banner.security.as_str(),
            mode.as_str()
        );
    }
    write_packet(
        stream,
        &SecurityClientHello {
            node_id: device_id,
            security: mode,
            protocol_version: sidewire_protocol::VERSION,
        },
    )
    .await?;
    let decision: SecurityDecision = read_packet(stream).await?;
    if !decision.accepted {
        bail!(decision.message);
    }
    let secret = match mode {
        SecurityMode::Secure => Some(
            paired_host_secret(pairs_dir, banner.node_id)?
                .with_context(|| format!("host {} is not paired", banner.node_id.short()))?,
        ),
        SecurityMode::Insecure => None,
    };
    let noise = if let Some(secret) = secret {
        let prologue = security_prologue(device_id, banner.node_id);
        Some(noise_initiator(stream, &secret, &prologue).await?)
    } else {
        None
    };
    Ok(ConnectionSecurity {
        peer_id: banner.node_id,
        noise,
        shared_secret: secret,
    })
}

fn pairing_pin(path: &str) -> Result<Option<String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let pin = text
        .lines()
        .find_map(|line| line.strip_prefix("pin="))
        .map(str::to_owned);
    let until = text
        .lines()
        .find_map(|line| line.strip_prefix("until="))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let Some(pin) = pin else {
        return Ok(None);
    };
    if now > until || pin.len() != 6 || !pin.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    Ok(Some(pin))
}

async fn handle_pair_connection(
    stream: &mut TcpStream,
    device_name: &str,
    device_id: DeviceId,
    pairing_file: &str,
    pairs_dir: &str,
) -> Result<()> {
    let pin = pairing_pin(pairing_file)?;
    write_packet(
        stream,
        &PairBanner {
            device_id,
            name: device_name.to_owned(),
            protocol_version: sidewire_protocol::VERSION,
            pairing_available: pin.is_some(),
        },
    )
    .await?;
    let pin = pin.context("pairing is not enabled or has expired")?;
    let start: PairStart = read_packet(stream).await?;
    let host_id_text = start.host_id.to_hex();
    let device_id_text = device_id.to_hex();
    let id_a = Identity::new(host_id_text.as_bytes());
    let id_b = Identity::new(device_id_text.as_bytes());
    let (state, message) = Spake2::<Ed25519Group>::start_b(&Password::new(pin), &id_a, &id_b);
    write_packet(
        stream,
        &PairReply {
            spake_message: message,
        },
    )
    .await?;
    let shared = state
        .finish(&start.spake_message)
        .map_err(|_| anyhow::anyhow!("pairing key exchange failed"))?;
    let pairing_key = pairing_psk(&shared, start.host_id, device_id);
    let prologue = security_prologue(start.host_id, device_id);
    let noise = noise_responder(stream, &pairing_key, &prologue)
        .await
        .context("pairing authentication failed")?;
    let commit_bytes = read_noise_record(stream, &noise).await?;
    let commit: PairCommit = decode(&commit_bytes)?;
    if commit.host_id != start.host_id {
        bail!("pairing host identity changed during handshake");
    }
    store_paired_host(
        pairs_dir,
        commit.host_id,
        &commit.host_name,
        commit.shared_secret,
    )?;
    let _ = fs::remove_file(pairing_file);
    let complete = encode(&PairComplete {
        device_id,
        device_name: device_name.to_owned(),
    })?;
    write_noise_record(stream, &noise, &complete).await?;
    tracing::info!(host = %commit.host_name, host_id = %commit.host_id.short(), "SideWire host paired");
    Ok(())
}

pub(super) async fn run_pairing_listener(
    port: u16,
    device_name: String,
    device_id: DeviceId,
    pairing_file: String,
    pairs_dir: String,
) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("bind SideWire pairing listener tcp:{port}"))?;
    tracing::info!(port, "SideWire pairing listener ready");
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let result = timeout(
            Duration::from_secs(65),
            handle_pair_connection(
                &mut stream,
                &device_name,
                device_id,
                &pairing_file,
                &pairs_dir,
            ),
        )
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%peer, %error, "pairing attempt failed");
                sleep(Duration::from_millis(300)).await;
            }
            Err(_) => {
                tracing::warn!(%peer, "pairing attempt timed out");
                sleep(Duration::from_millis(300)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_or_missing_pairing_file_is_inactive() {
        let path =
            std::env::temp_dir().join(format!("sidewire-pair-test-{}", rand::random::<u64>()));
        assert!(pairing_pin(path.to_str().unwrap()).unwrap().is_none());
    }
}
