//! Trait the player pushes decoded PCM into, plus the three built-in outputs.

use anyhow::Result;

/// Format hint the decoder pushes to the sink whenever the stream changes
/// sample rate or channel count. Interleaved S16LE is the only payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// Sink trait. Implementations must be `Send + Sync` — the player calls them
/// from a tokio task.
pub trait AudioSink: Send + Sync {
    /// Called once per format change. May be called multiple times.
    fn set_format(&self, fmt: PcmFormat) -> Result<()>;
    /// Interleaved S16LE samples. May be called frequently with small slices.
    fn write(&self, samples: &[i16]) -> Result<()>;
    /// Called when the player is stopping. Sinks should flush and close.
    fn close(&self) {}
}

/// Which output kind the gRPC caller asked for. Resolved to a concrete
/// `AudioSink` inside the player.
#[derive(Debug, Clone)]
pub enum AudioOutput {
    Cpal { device: Option<String> },
    Stdout,
    Pipe { path: String },
}
