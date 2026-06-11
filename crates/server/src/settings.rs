//! `zerod.toml` schema and loader.
//!
//! Example:
//!   [server]
//!   bind = "0.0.0.0:50151"
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
    pub mdns: MdnsSettings,
    #[serde(default)]
    pub configs: Vec<ManagedConfig>,
    #[serde(default)]
    pub snapcast: SnapcastSettings,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SnapcastSettings {
    /// Connect to a snapserver on startup and expose `SnapcastService`.
    /// When `false`, the service is still reachable but every RPC returns
    /// `FAILED_PRECONDITION` so reflection-based clients can see it exists.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_snap_host")]
    pub host: String,
    #[serde(default = "default_snap_port")]
    pub port: u16,
    /// Forward snapserver push notifications onto the in-process event bus.
    /// Useful so external `snapctl` changes still surface to `events tail`.
    #[serde(default = "default_true")]
    pub forward_notifications: bool,
}

impl Default for SnapcastSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: default_snap_host(),
            port: default_snap_port(),
            forward_notifications: true,
        }
    }
}

fn default_snap_host() -> String {
    "127.0.0.1".to_string()
}

fn default_snap_port() -> u16 {
    1705
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MdnsSettings {
    /// Advertise the daemon on the LAN via mDNS. Default: true.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Instance name to advertise. Empty → derive from the machine hostname.
    #[serde(default)]
    pub name: String,
}

impl Default for MdnsSettings {
    fn default() -> Self {
        Self { enabled: true, name: String::new() }
    }
}

fn default_true() -> bool {
    true
}

fn default_bind() -> String {
    // Bind on all interfaces by default so the daemon is reachable from the
    // LAN out of the box — bearer-token auth (random fallback) covers the
    // "no zerod.toml" case. Override to "127.0.0.1:50151" for loopback only.
    "0.0.0.0:50151".to_string()
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
            mdns: MdnsSettings::default(),
            configs: Vec::new(),
            snapcast: SnapcastSettings::default(),
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
        tracing::warn!("zerod.toml not found; using defaults (bind 0.0.0.0:50151, no allowlist, no configs)");
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
