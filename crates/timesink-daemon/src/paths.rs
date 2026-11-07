//! Where timesink keeps its files, and loading of the user's settings.
//!
//! The shared helpers live in `timesink_core::paths`; they are re-exported here
//! so the rest of the daemon keeps using `paths::...` unchanged.

use anyhow::Result;
use timesink_core::config::Config;
use timesink_core::sanitize::Sanitizer;

pub use timesink_core::paths::{
    config_dir, data_dir, db_path, device_id, load_app_map, load_config, read_config_file,
};

/// Build the privacy sanitizer from `hosts.conf` (ssh host labels) and the
/// settings in `config.conf`. Without hosts.conf every ssh host is recorded
/// as "ssh: other".
pub fn load_sanitizer(config: &Config) -> Result<Sanitizer> {
    let sanitizer = read_config_file("hosts.conf")?
        .map(|text| Sanitizer::from_hosts_conf(&text))
        .unwrap_or_default();
    Ok(sanitizer.with_config(config))
}
