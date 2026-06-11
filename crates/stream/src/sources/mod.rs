//! External playback sources. Each source eventually feeds the same
//! [`AudioSink`](crate::sink::AudioSink) trait so the server only has to
//! know about one sink-construction path.

use crate::sink::AudioOutput;

/// Configuration for the librespot subprocess source. Constructed by the
/// server from `[librespot]` in zerod.toml plus the per-RPC output choice.
#[derive(Debug, Clone)]
pub struct LibrespotConfig {
    /// Path or name of the `librespot` binary. Resolved against `$PATH`
    /// when not absolute.
    pub binary: String,
    /// Spotify Connect device name advertised to phones.
    pub name: String,
    /// 96 / 160 / 320 (kbps).
    pub bitrate: u32,
    /// Directory librespot uses for credentials/cache. Empty → librespot
    /// default (current directory). Disables on-disk audio caching.
    pub cache_path: String,
    /// Where decoded PCM goes.
    pub output: AudioOutput,
}

#[cfg(target_os = "linux")]
mod librespot;
#[cfg(target_os = "linux")]
pub use librespot::{spotify_start, spotify_stop};

#[cfg(not(target_os = "linux"))]
mod librespot_stub;
#[cfg(not(target_os = "linux"))]
pub use librespot_stub::{spotify_start, spotify_stop};
