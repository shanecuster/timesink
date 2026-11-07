//! Where timesink keeps its files, shared by the daemon and the terminal UI.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::appmap::AppMap;
use crate::config::Config;

/// Folder for data files: `$XDG_DATA_HOME/timesink` or `~/.local/share/timesink`.
pub fn data_dir() -> Result<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// Folder for config files: `$XDG_CONFIG_HOME/timesink` or `~/.config/timesink`.
pub fn config_dir() -> Result<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// Resolve an XDG base folder (or its `$HOME` fallback) and append `timesink`.
fn xdg_dir(var: &str, home_fallback: &str) -> Result<PathBuf> {
    let base = match std::env::var_os(var) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(home_fallback)
        }
    };
    Ok(base.join("timesink"))
}

/// Database path: `TIMESINK_DB` if set, otherwise the default data folder.
pub fn db_path() -> Result<PathBuf> {
    match std::env::var_os("TIMESINK_DB") {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(data_dir()?.join("timesink.db")),
    }
}

/// Name of this computer: `TIMESINK_DEVICE`, else /etc/hostname, else "unknown".
pub fn device_id() -> String {
    if let Ok(name) = std::env::var("TIMESINK_DEVICE") {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Read a config file from the config folder. A missing file is not an error.
pub fn read_config_file(name: &str) -> Result<Option<String>> {
    let path = config_dir()?.join(name);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Load `config.conf` (idle timeout etc.), falling back to the defaults.
pub fn load_config() -> Result<Config> {
    Ok(read_config_file("config.conf")?
        .map(|text| Config::from_conf(&text))
        .unwrap_or_default())
}

/// Load `apps.conf` (friendly names and categories). Without the file every
/// app keeps its raw name and is listed under "Other".
pub fn load_app_map() -> Result<AppMap> {
    Ok(read_config_file("apps.conf")?
        .map(|text| AppMap::from_conf(&text))
        .unwrap_or_default())
}

/// Where the interface remembers its choices: `tui.conf` in the config folder.
pub fn tui_settings_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("tui.conf"))
}

/// Where `apps.conf` lives (also where the interface appends entries).
pub fn apps_conf_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("apps.conf"))
}
