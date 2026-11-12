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
/// app keeps its raw name and is listed under "Other", except that well-known browsers
/// are shown as "Browser" (a line in the file always wins).
pub fn load_app_map() -> Result<AppMap> {
    Ok(read_config_file("apps.conf")?
        .map(|text| AppMap::from_conf(&text))
        .unwrap_or_default()
        .with_browser_defaults())
}

/// A config file's text and when it last changed (Unix seconds); empty text and 0 when the
/// file does not exist.
fn read_with_mtime(name: &str) -> Result<(String, i64)> {
    let path = config_dir()?.join(name);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let changed = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            Ok((text, changed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((String::new(), 0)),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// This computer's own lists (`apps.conf` and the site lines of `domains.conf`) with the
/// times they last changed, ready to be shared through sync.
pub fn own_conf_lists() -> Result<crate::syncdoc::ConfLists> {
    let (apps, apps_updated) = read_with_mtime("apps.conf")?;
    let (domains, domains_updated) = read_with_mtime("domains.conf")?;
    Ok(crate::syncdoc::ConfLists {
        apps,
        apps_updated,
        domains: crate::domains::DomainConfig::site_lines(&domains),
        domains_updated,
    })
}

/// Like [`load_app_map`], but also uses the names and categories other computers have
/// shared through sync (kept in the database). Your own file comes first; for the same app
/// whichever list changed most recently wins.
pub fn load_app_map_with(db: &crate::db::Db) -> Result<AppMap> {
    let own = own_conf_lists()?;
    let remote = db.remote_confs()?;
    let mut layers: Vec<(&str, i64)> = vec![(own.apps.as_str(), own.apps_updated)];
    layers.extend(
        remote
            .iter()
            .map(|(_, l)| (l.apps.as_str(), l.apps_updated)),
    );
    Ok(AppMap::from_layers(&layers).with_browser_defaults())
}

/// Like [`load_domain_config`], but with site names and categories shared by other
/// computers merged in. The browser and block settings are always only your own.
pub fn load_domain_config_with(db: &crate::db::Db) -> Result<crate::domains::DomainConfig> {
    let own = own_conf_lists()?;
    let config = load_domain_config()?;
    let remote = db.remote_confs()?;
    let mut layers: Vec<(&str, i64)> = vec![(own.domains.as_str(), own.domains_updated)];
    layers.extend(
        remote
            .iter()
            .map(|(_, l)| (l.domains.as_str(), l.domains_updated)),
    );
    Ok(config.with_site_layers(&layers))
}

/// Where the interface remembers its choices: `tui.conf` in the config folder.
pub fn tui_settings_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("tui.conf"))
}

/// Where `apps.conf` lives (also where the interface appends entries).
pub fn apps_conf_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("apps.conf"))
}

/// Where `sync.conf` lives: the CouchDB address and login. It holds a password, so it is
/// only ever read from here and should be readable by you alone (`chmod 600`).
pub fn sync_conf_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("sync.conf"))
}

/// Load `domains.conf` (browser class, blocked sites, site names and categories). Without
/// the file the defaults apply: the browser is `zen`, nothing is blocked.
pub fn load_domain_config() -> Result<crate::domains::DomainConfig> {
    Ok(read_config_file("domains.conf")?
        .map(|text| crate::domains::DomainConfig::from_conf(&text))
        .unwrap_or_default())
}
