use anyhow::{Context, Result, bail};
use sidewire_protocol::{
    DeviceId, PairBanner, PairCommit, PairComplete, PairReply, PairStart, SecurityBanner,
    SecurityClientHello, SecurityDecision, SecurityMode, SharedNoise, decode, encode, key_from_hex,
    key_to_hex, negotiated_version, noise_initiator, noise_responder, pairing_psk,
    protocol_compatible, protocol_label, read_noise_record, read_packet, security_prologue,
    write_noise_record, write_packet,
};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Mutex, Notify, Semaphore},
    time::{Duration, sleep, timeout},
};

const MAX_PAIRING_CONNECTIONS: usize = 4;
const MAX_PAIRING_ATTEMPTS_PER_IP: usize = 5;
const MAX_PIN_FAILURES: u8 = 5;
const PAIRING_RATE_WINDOW: StdDuration = StdDuration::from_secs(60);

#[derive(Default)]
struct PairingGuard {
    per_ip: HashMap<IpAddr, VecDeque<Instant>>,
    current_pin: Option<String>,
    failures: u8,
}

impl PairingGuard {
    fn allow_ip(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let attempts = self.per_ip.entry(ip).or_default();
        while attempts
            .front()
            .is_some_and(|at| now.duration_since(*at) > PAIRING_RATE_WINDOW)
        {
            attempts.pop_front();
        }
        if attempts.len() >= MAX_PAIRING_ATTEMPTS_PER_IP {
            return false;
        }
        attempts.push_back(now);
        true
    }

    fn clear_success(&mut self) {
        self.current_pin = None;
        self.failures = 0;
    }
}

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
    if !protocol_compatible(hello.protocol_version) {
        return reject(
            stream,
            format!(
                "protocol mismatch: host {}, device {}",
                protocol_label(hello.protocol_version),
                protocol_label(sidewire_protocol::VERSION)
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
        let version =
            negotiated_version(hello.protocol_version).context("no compatible protocol version")?;
        let prologue = security_prologue(version, hello.node_id, device_id);
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
    if !protocol_compatible(banner.protocol_version) {
        bail!(
            "protocol mismatch: host {}, device {}",
            protocol_label(banner.protocol_version),
            protocol_label(sidewire_protocol::VERSION)
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
        let version = negotiated_version(banner.protocol_version)
            .context("no compatible protocol version")?;
        let prologue = security_prologue(version, device_id, banner.node_id);
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

async fn record_pair_failure(pairing_file: &str, guard: &Arc<Mutex<PairingGuard>>) -> Result<bool> {
    let Some(pin) = pairing_pin(pairing_file)? else {
        return Ok(false);
    };
    let mut guard = guard.lock().await;
    if guard.current_pin.as_deref() != Some(pin.as_str()) {
        guard.current_pin = Some(pin);
        guard.failures = 0;
    }
    guard.failures = guard.failures.saturating_add(1);
    if guard.failures >= MAX_PIN_FAILURES {
        let _ = fs::remove_file(pairing_file);
        guard.current_pin = None;
        guard.failures = 0;
        return Ok(true);
    }
    Ok(false)
}

async fn handle_pair_connection(
    stream: &mut TcpStream,
    device_name: &str,
    device_id: DeviceId,
    pairing_file: &str,
    pairs_dir: &str,
    commit_lock: &Arc<Mutex<()>>,
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
    if !protocol_compatible(start.protocol_version) {
        bail!("pairing protocol major mismatch");
    }
    let protocol = negotiated_version(start.protocol_version)
        .context("no compatible pairing protocol version")?;
    let host_id_text = start.host_id.to_hex();
    let device_id_text = device_id.to_hex();
    let id_a = Identity::new(host_id_text.as_bytes());
    let id_b = Identity::new(device_id_text.as_bytes());
    let (state, message) =
        Spake2::<Ed25519Group>::start_b(&Password::new(pin.as_bytes()), &id_a, &id_b);
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
    let prologue = security_prologue(protocol, start.host_id, device_id);
    let noise = noise_responder(stream, &pairing_key, &prologue)
        .await
        .context("pairing authentication failed")?;
    let commit_bytes = read_noise_record(stream, &noise).await?;
    let commit: PairCommit = decode(&commit_bytes)?;
    if commit.host_id != start.host_id {
        bail!("pairing host identity changed during handshake");
    }
    let _commit_guard = commit_lock.lock().await;
    if pairing_pin(pairing_file)?.as_deref() != Some(pin.as_str()) {
        bail!("pairing PIN was already used or expired");
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
    reconnect: Option<Arc<Notify>>,
) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("bind SideWire pairing listener tcp:{port}"))?;
    let permits = Arc::new(Semaphore::new(MAX_PAIRING_CONNECTIONS));
    let guard = Arc::new(Mutex::new(PairingGuard::default()));
    let commit_lock = Arc::new(Mutex::new(()));
    tracing::info!(port, "SideWire pairing listener ready");
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let allowed = guard.lock().await.allow_ip(peer.ip());
        if !allowed {
            tracing::warn!(%peer, "pairing rate limit exceeded");
            continue;
        }
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            tracing::warn!(%peer, "too many concurrent pairing attempts");
            continue;
        };
        let _ = stream.set_nodelay(true);
        let name = device_name.clone();
        let pairing_file = pairing_file.clone();
        let pairs_dir = pairs_dir.clone();
        let guard = guard.clone();
        let commit_lock = commit_lock.clone();
        let reconnect = reconnect.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result = timeout(
                Duration::from_secs(65),
                handle_pair_connection(
                    &mut stream,
                    &name,
                    device_id,
                    &pairing_file,
                    &pairs_dir,
                    &commit_lock,
                ),
            )
            .await;
            match result {
                Ok(Ok(())) => {
                    guard.lock().await.clear_success();
                    if let Some(reconnect) = reconnect {
                        reconnect.notify_waiters();
                    }
                }
                Ok(Err(error)) => {
                    tracing::warn!(%peer, %error, "pairing attempt failed");
                    if record_pair_failure(&pairing_file, &guard)
                        .await
                        .unwrap_or(false)
                    {
                        tracing::warn!("pairing PIN disabled after too many failed attempts");
                    }
                    sleep(Duration::from_millis(300)).await;
                }
                Err(_) => {
                    tracing::warn!(%peer, "pairing attempt timed out");
                    if record_pair_failure(&pairing_file, &guard)
                        .await
                        .unwrap_or(false)
                    {
                        tracing::warn!("pairing PIN disabled after too many failed attempts");
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_guard_rate_limits_an_ip() {
        let mut guard = PairingGuard::default();
        let ip: std::net::IpAddr = "192.0.2.10".parse().unwrap();
        for _ in 0..MAX_PAIRING_ATTEMPTS_PER_IP {
            assert!(guard.allow_ip(ip));
        }
        assert!(!guard.allow_ip(ip));
        assert!(guard.allow_ip("192.0.2.11".parse().unwrap()));
    }

    #[test]
    fn active_pairing_file_is_available() {
        let path =
            std::env::temp_dir().join(format!("sidewire-pair-active-{}", rand::random::<u64>()));
        let until = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        fs::write(&path, format!("pin=123456\nuntil={until}\n")).unwrap();
        assert_eq!(
            pairing_pin(path.to_str().unwrap()).unwrap().as_deref(),
            Some("123456")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn expired_or_missing_pairing_file_is_inactive() {
        let path =
            std::env::temp_dir().join(format!("sidewire-pair-test-{}", rand::random::<u64>()));
        assert!(pairing_pin(path.to_str().unwrap()).unwrap().is_none());
    }
}
