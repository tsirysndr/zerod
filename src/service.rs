//! `zerod service {install,uninstall,path}` — manage zerod's own systemd
//! user-unit. Linux-only: on other platforms the CLI surfaces a clear error
//! rather than producing a unit file no one can use.

#[cfg(target_os = "linux")]
mod imp {
    use anyhow::{anyhow, Context, Result};
    use std::path::PathBuf;

    const TEMPLATE: &str = include_str!("../assets/zerod.service.in");
    const UNIT_NAME: &str = "zerod.service";

    pub struct Installed {
        pub path: PathBuf,
        pub token: String,
    }

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

    pub fn install(force: bool, token: Option<String>) -> Result<Installed> {
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
                "{} already exists — pass --force to overwrite (this rotates the bearer token unless --token is given)",
                path.display()
            ));
        }

        let token = match token {
            Some(t) if !t.is_empty() => t,
            _ => random_hex_token()?,
        };

        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("unit path has no parent: {}", path.display()))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;

        let rendered = TEMPLATE
            .replace("{{exec_start}}", exe_str)
            .replace("{{bearer_token}}", &token);
        let tmp = path.with_extension("service.tmp");
        std::fs::write(&tmp, rendered)
            .with_context(|| format!("write {}", tmp.display()))?;
        // The unit file holds a secret. Tighten perms before publishing it.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;

        Ok(Installed { path, token })
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

    fn random_hex_token() -> Result<String> {
        let mut buf = [0u8; 32];
        getrandom::getrandom(&mut buf).context("getrandom for bearer token")?;
        let mut s = String::with_capacity(buf.len() * 2);
        for b in buf {
            use std::fmt::Write;
            write!(&mut s, "{b:02x}").unwrap();
        }
        Ok(s)
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use anyhow::{bail, Result};
    use std::path::PathBuf;

    pub struct Installed {
        pub path: PathBuf,
        pub token: String,
    }

    pub fn unit_path() -> Result<PathBuf> {
        bail!("zerod service: linux only")
    }
    pub fn install(_force: bool, _token: Option<String>) -> Result<Installed> {
        bail!("zerod service: linux only")
    }
    pub fn uninstall() -> Result<Option<PathBuf>> {
        bail!("zerod service: linux only")
    }
}

pub use imp::{install, uninstall, unit_path};
