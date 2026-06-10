//! Standalone HLS / DASH audio player with pluggable PCM sinks.
//!
//!     URL (.m3u8 / .mpd)
//!         → manifest parser (m3u8-rs / dash-mpd)
//!             → segment fetcher (reqwest, concurrent prefetch)
//!                 → demux + decode (symphonia: fMP4 / TS → AAC → S16LE PCM)
//!                     → AudioSink (cpal / stdout / pipe)
//!
//! Lifted from rockbox-zig/crates/hls and rewired to a trait-based output so
//! the gRPC layer can pick `cpal | stdout | pipe` per-stream at runtime.

mod cpal_sink;
mod decoder;
mod demux;
mod fetcher;
mod manifest;
mod output;
mod player;
mod sink;

pub use manifest::{is_hls_or_dash_url, ManifestKind};
pub use player::{
    pause, play, resume, set_volume, status, stop, volume, PlayConfig, PlayerState, Status,
};
pub use sink::{AudioOutput, AudioSink};
