use super::*;
use sidewire_protocol::{DeviceId, key_from_hex, key_to_hex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TrustedDevice {
    pub id: DeviceId,
    pub name: String,
    pub shared_secret: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustStore {
    host_id: Option<DeviceId>,
    devices: HashMap<String, TrustedDevice>,
}

fn trust_path() -> Result<PathBuf> {
    let config = crate::config::config_path()?;
    Ok(config.with_file_name("trust.json"))
}

fn load_store() -> Result<TrustStore> {
    let path = trust_path()?;
    if !path.exists() {
        return Ok(TrustStore::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read SideWire trust store {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parse SideWire trust store {}", path.display()))
}
fn save_store(store: &TrustStore) -> Result<()> {
    let path = trust_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(store)?;
    std::fs::write(&path, format!("{text}\n"))
        .with_context(|| format!("write SideWire trust store {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub(super) fn host_id() -> Result<DeviceId> {
    let mut store = load_store()?;
    if let Some(id) = store.host_id {
        return Ok(id);
    }
    let id = DeviceId(rand::random::<[u8; 16]>());
    store.host_id = Some(id);
    save_store(&store)?;
    Ok(id)
}

pub(super) fn host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "sidewire-host".into())
}
pub(super) fn device_secret(id: DeviceId) -> Result<Option<[u8; 32]>> {
    let store = load_store()?;
    store
        .devices
        .get(&id.to_hex())
        .map(|device| key_from_hex(&device.shared_secret))
        .transpose()
}

pub(super) fn save_device(id: DeviceId, name: String, secret: [u8; 32]) -> Result<()> {
    let mut store = load_store()?;
    store.devices.insert(
        id.to_hex(),
        TrustedDevice {
            id,
            name,
            shared_secret: key_to_hex(&secret),
        },
    );
    save_store(&store)
}

pub(super) fn list_devices() -> Result<Vec<TrustedDevice>> {
    let store = load_store()?;
    let mut devices: Vec<_> = store.devices.into_values().collect();
    devices.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.id.to_hex().cmp(&b.id.to_hex()))
    });
    Ok(devices)
}
pub(super) fn remove_device(selector: &str) -> Result<TrustedDevice> {
    let mut store = load_store()?;
    let selector = selector.trim().to_ascii_lowercase();
    let matches: Vec<String> = store
        .devices
        .iter()
        .filter(|(id, device)| {
            id.starts_with(&selector) || device.name.eq_ignore_ascii_case(&selector)
        })
        .map(|(id, _)| id.clone())
        .collect();
    match matches.len() {
        0 => bail!("paired device '{selector}' not found"),
        1 => {
            let removed = store.devices.remove(&matches[0]).unwrap();
            save_store(&store)?;
            Ok(removed)
        }
        _ => bail!("paired device selector '{selector}' is ambiguous; use an ID prefix"),
    }
}
