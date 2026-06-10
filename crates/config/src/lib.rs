//! Managed config-file I/O. Each entry maps a logical key (e.g. "snapserver")
//! to an absolute filesystem path plus an optional systemd unit to kick after
//! a write. Writes are atomic (tempfile + rename) so a partial write can never
//! corrupt the on-disk config.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zerod_systemd::UnitAllowlist;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedConfig {
    pub key: String,
    pub path: PathBuf,
    /// Systemd unit bound to this config. Empty string means "no unit"; the
    /// `reload`/`restart` post-write actions are no-ops in that case.
    #[serde(default)]
    pub unit: String,
}

#[derive(Debug, Clone, Copy)]
pub enum PostWriteAction {
    None,
    Reload,
    Restart,
}

#[derive(Default)]
pub struct Registry {
    by_key: HashMap<String, ManagedConfig>,
}

impl Registry {
    pub fn from_entries(entries: Vec<ManagedConfig>) -> Self {
        let mut by_key = HashMap::with_capacity(entries.len());
        for e in entries {
            by_key.insert(e.key.clone(), e);
        }
        Self { by_key }
    }

    pub fn list(&self) -> Vec<ManagedConfig> {
        let mut v: Vec<_> = self.by_key.values().cloned().collect();
        v.sort_by(|a, b| a.key.cmp(&b.key));
        v
    }

    pub fn get(&self, key: &str) -> Result<&ManagedConfig> {
        self.by_key
            .get(key)
            .ok_or_else(|| anyhow!("config: key {key} not in registry"))
    }

    pub async fn read(&self, key: &str) -> Result<(ManagedConfig, String)> {
        let mc = self.get(key)?.clone();
        let content = tokio::fs::read_to_string(&mc.path)
            .await
            .with_context(|| format!("read {}", mc.path.display()))?;
        Ok((mc, content))
    }

    /// Atomic write to the managed file. Preserves the existing file's mode
    /// bits when possible.
    pub async fn write(&self, key: &str, content: &str) -> Result<ManagedConfig> {
        let mc = self.get(key)?.clone();
        let parent = mc
            .path
            .parent()
            .ok_or_else(|| anyhow!("config: {} has no parent dir", mc.path.display()))?;
        atomic_write(parent, &mc.path, content).await?;
        Ok(mc)
    }
}

async fn atomic_write(dir: &Path, target: &Path, content: &str) -> Result<()> {
    let mode = tokio::fs::metadata(target)
        .await
        .ok()
        .map(|m| m.permissions().mode())
        .unwrap_or(0o644);
    let pid = std::process::id();
    let nonce = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("zerod-config");
    let tmp = dir.join(format!(".{nonce}.zerod.{pid}.tmp"));
    tokio::fs::write(&tmp, content)
        .await
        .with_context(|| format!("write tmp {}", tmp.display()))?;
    let perms = std::fs::Permissions::from_mode(mode & 0o7777);
    tokio::fs::set_permissions(&tmp, perms)
        .await
        .with_context(|| format!("chmod tmp {}", tmp.display()))?;
    tokio::fs::rename(&tmp, target)
        .await
        .with_context(|| format!("rename {} → {}", tmp.display(), target.display()))?;
    Ok(())
}

/// Apply the post-write action against systemd. Returns Ok(true) when the
/// action actually ran, Ok(false) for `None`. Errors propagate from systemd.
pub async fn apply_action(
    mc: &ManagedConfig,
    action: PostWriteAction,
    allow: &Arc<UnitAllowlist>,
) -> Result<bool> {
    if mc.unit.is_empty() {
        tracing::debug!("config: {} has no bound unit; skipping post-write action", mc.key);
        return Ok(false);
    }
    match action {
        PostWriteAction::None => Ok(false),
        PostWriteAction::Reload => {
            tracing::info!("config: reloading {} after write to {}", mc.unit, mc.key);
            zerod_systemd::reload(allow, &mc.unit).await?;
            Ok(true)
        }
        PostWriteAction::Restart => {
            tracing::info!("config: restarting {} after write to {}", mc.unit, mc.key);
            zerod_systemd::restart(allow, &mc.unit).await?;
            Ok(true)
        }
    }
}
