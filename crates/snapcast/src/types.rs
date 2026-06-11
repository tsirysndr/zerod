//! JSON shapes returned by `Server.GetStatus`. Fields use `#[serde(default)]`
//! so a schema bump in snapserver doesn't break parsing — missing fields
//! land at their default rather than failing the whole call.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct ServerStatusResult {
    pub server: SnapServer,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SnapServer {
    pub groups: Vec<SnapGroup>,
    pub streams: Vec<SnapStream>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SnapGroup {
    pub id: String,
    pub name: String,
    pub stream_id: String,
    pub muted: bool,
    pub clients: Vec<SnapClient>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SnapClient {
    pub id: String,
    pub connected: bool,
    pub host: SnapHost,
    pub config: SnapClientConfig,
    pub snapclient: SnapClientMeta,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SnapHost {
    pub name: String,
    pub mac: String,
    pub ip: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SnapClientConfig {
    pub name: String,
    pub volume: SnapVolume,
    pub latency: i32,
    pub instance: u32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SnapVolume {
    pub percent: u32,
    pub muted: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SnapClientMeta {
    pub version: String,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SnapStream {
    pub id: String,
    pub status: String,
}

impl SnapClient {
    /// Best-effort display name: explicit config.name first, falling back to
    /// host name, then the client id.
    pub fn display_name(&self) -> &str {
        if !self.config.name.is_empty() {
            &self.config.name
        } else if !self.host.name.is_empty() {
            &self.host.name
        } else {
            &self.id
        }
    }
}
