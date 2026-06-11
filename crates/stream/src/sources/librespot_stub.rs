//! Non-Linux placeholder. librespot itself runs on macOS too, but the
//! production target for zerod is Linux audio appliances — the stub
//! keeps non-Linux dev builds compiling without pulling in a working
//! librespot binary requirement.

use super::LibrespotConfig;
use anyhow::{bail, Result};

pub fn spotify_start(_cfg: LibrespotConfig) -> Result<()> {
    bail!("librespot: linux only")
}

pub fn spotify_stop() -> bool {
    false
}
