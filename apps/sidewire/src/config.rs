use super::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct AppConfig {
    pub connect: Option<String>,
    pub run_as: Option<RunAs>,
}

pub(super) fn config_path() -> Result<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .context("APPDATA is not set")?;
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("HOME/XDG_CONFIG_HOME is not set")?;

    #[cfg(windows)]
    return Ok(base.join("SideWire").join("config.json"));
    #[cfg(not(windows))]
    Ok(base.join("sidewire").join("config.json"))
}

pub(super) fn load() -> Result<AppConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read SideWire config {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse SideWire config {}", path.display()))
}

fn save(config: &AppConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, format!("{text}\n"))
        .with_context(|| format!("write SideWire config {}", path.display()))
}

pub(super) fn resolve_run_as(
    config: &AppConfig,
    requested: Option<RunAs>,
    fallback: RunAs,
) -> RunAs {
    requested.or(config.run_as).unwrap_or(fallback)
}

pub(super) fn show() -> Result<()> {
    let path = config_path()?;
    let config = load()?;
    println!("path={}", path.display());
    println!("connect={}", config.connect.as_deref().unwrap_or(""));
    println!(
        "run-as={}",
        match config.run_as {
            Some(RunAs::Root) => "root",
            Some(RunAs::Shell) => "shell",
            None => "",
        }
    );
    Ok(())
}

pub(super) fn set(key: &str, value: &str) -> Result<()> {
    let mut config = load()?;
    match key {
        "connect" => config.connect = Some(value.trim().to_owned()),
        "run-as" => {
            config.run_as = Some(match value.trim().to_ascii_lowercase().as_str() {
                "root" => RunAs::Root,
                "shell" => RunAs::Shell,
                _ => bail!("run-as must be root or shell"),
            });
        }
        _ => bail!("unknown config key '{key}' (supported: connect, run-as)"),
    }
    save(&config)?;
    println!("saved {}", config_path()?.display());
    Ok(())
}

pub(super) fn unset(key: &str) -> Result<()> {
    let mut config = load()?;
    match key {
        "connect" => config.connect = None,
        "run-as" => config.run_as = None,
        _ => bail!("unknown config key '{key}' (supported: connect, run-as)"),
    }
    save(&config)?;
    println!("saved {}", config_path()?.display());
    Ok(())
}
