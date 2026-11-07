//! Where timesink keeps its files, and loading of the user's settings.

use std::path::PathBuf;

use anyhow::{Context, Result};
use timesink_core::sanitize::Sanitizer;

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
fn read_config_file(name: &str) -> Result<Option<String>> {
    let path = config_dir()?.join(name);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Load `hosts.conf`, which maps ssh hosts to friendly labels.
/// Without the file every ssh host is recorded as "ssh: other".
pub fn load_sanitizer() -> Result<Sanitizer> {
    Ok(read_config_file("hosts.conf")?
        .map(|text| Sanitizer::from_hosts_conf(&text))
        .unwrap_or_default())
}
