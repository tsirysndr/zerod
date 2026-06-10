//! `zerod.toml` schema and loader.
//!
//! Example:
//!   [server]
//!   bind = "127.0.0.1:50151"
//!   bearer_token = ""
//!
//!   [systemd]
//!   units = ["snapserver.service", "shairport-sync.service", "squeezelite.service"]
//!
//!   [[configs]]
//!   key = "snapserver"
//!   path = "/etc/snapserver.conf"
//!   unit = "snapserver.service"

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zerod_config::{ManagedConfig, Registry};
use zerod_systemd::UnitAllowlist;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Settings {
    #[serde(default)]
    pub server: ServerSettings,
    #[serde(default)]
    pub systemd: SystemdSettings,
    #[serde(default)]
    pub configs: Vec<ManagedConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerSettings {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default)]
    pub bearer_token: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SystemdSettings {
    #[serde(default)]
    pub units: Vec<String>,
}

fn default_bind() -> String {
    "127.0.0.1:50151".to_string()
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            bearer_token: String::new(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server: ServerSettings::default(),
            systemd: SystemdSettings::default(),
            configs: Vec::new(),
        }
    }
}

impl Settings {
    pub fn systemd_allowlist(&self) -> UnitAllowlist {
        UnitAllowlist {
            units: self.systemd.units.clone(),
        }
    }

    pub fn config_registry(&self) -> Registry {
        Registry::from_entries(self.configs.clone())
    }
}

/// Load a `zerod.toml`. If `path` is `None`, search `./zerod.toml`,
/// `$XDG_CONFIG_HOME/zerod/zerod.toml`, `/etc/zerod.toml` in order. If
/// nothing is found, return defaults and warn.
pub fn load_settings(path: Option<&Path>) -> Result<Settings> {
    let resolved: Option<PathBuf> = match path {
        Some(p) => Some(p.to_path_buf()),
        None => default_search_paths().into_iter().find(|p| p.exists()),
    };
    let Some(p) = resolved else {
        tracing::warn!("zerod.toml not found; using defaults (loopback, no allowlist, no configs)");
        return Ok(Settings::default());
    };
    let body = std::fs::read_to_string(&p)
        .with_context(|| format!("read {}", p.display()))?;
    let settings: Settings = toml::from_str(&body)
        .with_context(|| format!("parse {}", p.display()))?;
    tracing::info!("loaded settings from {}", p.display());
    Ok(settings)
}

fn default_search_paths() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("zerod.toml")];
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        out.push(PathBuf::from(xdg).join("zerod").join("zerod.toml"));
    } else if let Ok(home) = std::env::var("HOME") {
        out.push(
            PathBuf::from(home)
                .join(".config")
                .join("zerod")
                .join("zerod.toml"),
        );
    }
    out.push(PathBuf::from("/etc/zerod.toml"));
    out
}
