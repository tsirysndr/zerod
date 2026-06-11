//! Linux systemd control via zbus, with an allowlist. Non-Linux builds expose
//! the same surface but every call returns `Err("systemd: linux only")`.
//!
//! The allowlist exists so this gRPC service can't be turned into a generic
//! "remote systemctl" — only configured units accept actions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveState {
    Active,
    Reloading,
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Unknown,
}

impl ActiveState {
    pub fn parse(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "reloading" => Self::Reloading,
            "inactive" => Self::Inactive,
            "failed" => Self::Failed,
            "activating" => Self::Activating,
            "deactivating" => Self::Deactivating,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitStatus {
    pub name: String,
    pub description: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub enabled: bool,
}

/// Allowlist of unit names the gRPC layer will accept. Resolved from CLI
/// flags / a config file at startup.
#[derive(Debug, Clone, Default)]
pub struct UnitAllowlist {
    pub units: Vec<String>,
}

impl UnitAllowlist {
    pub fn check(&self, name: &str) -> anyhow::Result<()> {
        if self.units.iter().any(|u| u == name) {
            Ok(())
        } else {
            Err(anyhow::anyhow!("systemd: unit {name} not in allowlist"))
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{ActiveState, UnitAllowlist, UnitStatus};
    use anyhow::{Context, Result};

    fn publish_unit(u: &UnitStatus) {
        zerod_events::publish(zerod_events::Event::SystemdUnitState {
            name: u.name.clone(),
            active_state: u.active_state.clone(),
            sub_state: u.sub_state.clone(),
            enabled: u.enabled,
        });
    }

    async fn publish_after(allow: &UnitAllowlist, name: &str) {
        if let Ok(s) = status(allow, name).await {
            publish_unit(&s);
        }
    }

    #[zbus::proxy(
        interface = "org.freedesktop.systemd1.Manager",
        default_service = "org.freedesktop.systemd1",
        default_path = "/org/freedesktop/systemd1"
    )]
    trait Manager {
        async fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
        async fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
        async fn restart_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
        async fn reload_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
        async fn enable_unit_files(
            &self,
            files: &[&str],
            runtime: bool,
            force: bool,
        ) -> zbus::Result<(bool, Vec<(String, String, String)>)>;
        async fn disable_unit_files(
            &self,
            files: &[&str],
            runtime: bool,
        ) -> zbus::Result<Vec<(String, String, String)>>;
        async fn reload(&self) -> zbus::Result<()>;
        async fn get_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
        async fn load_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
        async fn get_unit_file_state(&self, file: &str) -> zbus::Result<String>;
    }

    #[zbus::proxy(
        interface = "org.freedesktop.systemd1.Unit",
        default_service = "org.freedesktop.systemd1"
    )]
    trait Unit {
        #[zbus(property)]
        fn id(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn description(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn load_state(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn active_state(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn sub_state(&self) -> zbus::Result<String>;
    }

    async fn connect() -> Result<(zbus::Connection, ManagerProxy<'static>)> {
        let conn = zbus::Connection::system()
            .await
            .context("zbus: connect to system bus")?;
        let mgr = ManagerProxy::new(&conn)
            .await
            .context("zbus: build Manager proxy")?;
        Ok((conn, mgr))
    }

    pub async fn status(allow: &UnitAllowlist, name: &str) -> Result<UnitStatus> {
        allow.check(name)?;
        let (conn, mgr) = connect().await?;
        let path = mgr
            .load_unit(name)
            .await
            .with_context(|| format!("load_unit {name}"))?;
        let unit = UnitProxy::builder(&conn)
            .path(path)
            .context("zbus: build Unit path")?
            .build()
            .await
            .context("zbus: build Unit proxy")?;
        let enabled = mgr
            .get_unit_file_state(name)
            .await
            .map(|s| matches!(s.as_str(), "enabled" | "enabled-runtime" | "alias" | "static"))
            .unwrap_or(false);
        Ok(UnitStatus {
            name: unit.id().await.unwrap_or_else(|_| name.to_string()),
            description: unit.description().await.unwrap_or_default(),
            load_state: unit.load_state().await.unwrap_or_default(),
            active_state: unit.active_state().await.unwrap_or_default(),
            sub_state: unit.sub_state().await.unwrap_or_default(),
            enabled,
        })
    }

    pub async fn list(allow: &UnitAllowlist) -> Result<Vec<UnitStatus>> {
        let mut out = Vec::with_capacity(allow.units.len());
        for name in &allow.units {
            match status(allow, name).await {
                Ok(u) => out.push(u),
                Err(e) => {
                    tracing::warn!("systemd: status {name}: {e}");
                    out.push(UnitStatus {
                        name: name.clone(),
                        description: String::new(),
                        load_state: "not-found".into(),
                        active_state: "unknown".into(),
                        sub_state: String::new(),
                        enabled: false,
                    });
                }
            }
        }
        Ok(out)
    }

    pub async fn start(allow: &UnitAllowlist, name: &str) -> Result<()> {
        allow.check(name)?;
        let (_c, mgr) = connect().await?;
        mgr.start_unit(name, "replace").await.with_context(|| format!("start {name}"))?;
        publish_after(allow, name).await;
        Ok(())
    }
    pub async fn stop(allow: &UnitAllowlist, name: &str) -> Result<()> {
        allow.check(name)?;
        let (_c, mgr) = connect().await?;
        mgr.stop_unit(name, "replace").await.with_context(|| format!("stop {name}"))?;
        publish_after(allow, name).await;
        Ok(())
    }
    pub async fn restart(allow: &UnitAllowlist, name: &str) -> Result<()> {
        allow.check(name)?;
        let (_c, mgr) = connect().await?;
        mgr.restart_unit(name, "replace").await.with_context(|| format!("restart {name}"))?;
        publish_after(allow, name).await;
        Ok(())
    }
    pub async fn reload(allow: &UnitAllowlist, name: &str) -> Result<()> {
        allow.check(name)?;
        let (_c, mgr) = connect().await?;
        mgr.reload_unit(name, "replace").await.with_context(|| format!("reload {name}"))?;
        publish_after(allow, name).await;
        Ok(())
    }
    pub async fn enable(allow: &UnitAllowlist, name: &str) -> Result<()> {
        allow.check(name)?;
        let (_c, mgr) = connect().await?;
        mgr.enable_unit_files(&[name], false, false)
            .await
            .with_context(|| format!("enable {name}"))?;
        // EnableUnitFiles doesn't apply changes until daemon-reload.
        mgr.reload().await.ok();
        publish_after(allow, name).await;
        Ok(())
    }
    pub async fn disable(allow: &UnitAllowlist, name: &str) -> Result<()> {
        allow.check(name)?;
        let (_c, mgr) = connect().await?;
        mgr.disable_unit_files(&[name], false)
            .await
            .with_context(|| format!("disable {name}"))?;
        mgr.reload().await.ok();
        publish_after(allow, name).await;
        Ok(())
    }

    /// Helper used by the config crate after a successful PutConfig.
    pub async fn daemon_reload() -> Result<()> {
        let (_c, mgr) = connect().await?;
        mgr.reload().await.context("daemon-reload")?;
        Ok(())
    }

    // Re-export so callers can build ActiveState from the active_state string
    // without pulling zbus into their scope.
    pub use super::ActiveState as _ActiveState;
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{UnitAllowlist, UnitStatus};
    use anyhow::{bail, Result};

    pub async fn status(_a: &UnitAllowlist, _n: &str) -> Result<UnitStatus> {
        bail!("systemd: linux only")
    }
    pub async fn list(_a: &UnitAllowlist) -> Result<Vec<UnitStatus>> {
        bail!("systemd: linux only")
    }
    pub async fn start(_a: &UnitAllowlist, _n: &str) -> Result<()> { bail!("systemd: linux only") }
    pub async fn stop(_a: &UnitAllowlist, _n: &str) -> Result<()> { bail!("systemd: linux only") }
    pub async fn restart(_a: &UnitAllowlist, _n: &str) -> Result<()> { bail!("systemd: linux only") }
    pub async fn reload(_a: &UnitAllowlist, _n: &str) -> Result<()> { bail!("systemd: linux only") }
    pub async fn enable(_a: &UnitAllowlist, _n: &str) -> Result<()> { bail!("systemd: linux only") }
    pub async fn disable(_a: &UnitAllowlist, _n: &str) -> Result<()> { bail!("systemd: linux only") }
    pub async fn daemon_reload() -> Result<()> { bail!("systemd: linux only") }
}

pub use imp::{daemon_reload, disable, enable, list, reload, restart, start, status, stop};
