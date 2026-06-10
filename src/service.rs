//! `zerod service {install,uninstall,path}` — manage zerod's own systemd
//! user-unit. Linux-only: on other platforms the CLI surfaces a clear error
//! rather than producing a unit file no one can use.

#[cfg(target_os = "linux")]
mod imp {
    use anyhow::{anyhow, Context, Result};
    use std::path::PathBuf;

    const TEMPLATE: &str = include_str!("../assets/zerod.service.in");
    const UNIT_NAME: &str = "zerod.service";

    /// `$XDG_CONFIG_HOME/systemd/user/zerod.service` or, when XDG isn't
    /// set, `~/.config/systemd/user/zerod.service`.
    pub fn unit_path() -> Result<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => {
                let home = std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| anyhow!("HOME is not set"))?;
                PathBuf::from(home).join(".config")
            }
        };
        Ok(base.join("systemd").join("user").join(UNIT_NAME))
    }

    pub fn install(force: bool) -> Result<PathBuf> {
        let exe = std::env::current_exe().context("resolve current_exe()")?;
        let exe = exe
            .canonicalize()
            .unwrap_or(exe); // best-effort canonicalise; symlinks → real path
        let exe_str = exe
            .to_str()
            .ok_or_else(|| anyhow!("current_exe path is not UTF-8: {exe:?}"))?;

        let path = unit_path()?;
        if path.exists() && !force {
            return Err(anyhow!(
                "{} already exists — pass --force to overwrite",
                path.display()
            ));
        }

        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("unit path has no parent: {}", path.display()))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;

        let rendered = TEMPLATE.replace("{{exec_start}}", exe_str);
        let tmp = path.with_extension("service.tmp");
        std::fs::write(&tmp, rendered)
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;

        Ok(path)
    }

    pub fn uninstall() -> Result<Option<PathBuf>> {
        let path = unit_path()?;
        if !path.exists() {
            return Ok(None);
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("rm {}", path.display()))?;
        Ok(Some(path))
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use anyhow::{bail, Result};
    use std::path::PathBuf;

    pub fn unit_path() -> Result<PathBuf> {
        bail!("zerod service: linux only")
    }
    pub fn install(_force: bool) -> Result<PathBuf> {
        bail!("zerod service: linux only")
    }
    pub fn uninstall() -> Result<Option<PathBuf>> {
        bail!("zerod service: linux only")
    }
}

pub use imp::{install, uninstall, unit_path};
