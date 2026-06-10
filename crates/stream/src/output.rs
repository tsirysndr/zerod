//! Stdout and named-pipe sinks. Both emit raw interleaved S16LE little-endian
//! PCM — the consumer is expected to know sample rate / channels out of band.

use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

use crate::sink::{AudioSink, PcmFormat};

pub struct StdoutSink {
    fmt: Mutex<Option<PcmFormat>>,
}

impl StdoutSink {
    pub fn new() -> Self {
        Self {
            fmt: Mutex::new(None),
        }
    }
}

impl AudioSink for StdoutSink {
    fn set_format(&self, fmt: PcmFormat) -> Result<()> {
        let mut g = self.fmt.lock().unwrap();
        if g.map_or(true, |old| old.sample_rate != fmt.sample_rate || old.channels != fmt.channels)
        {
            tracing::info!(
                "stream/stdout: format {} Hz × {} ch (S16LE)",
                fmt.sample_rate,
                fmt.channels
            );
            *g = Some(fmt);
        }
        Ok(())
    }

    fn write(&self, samples: &[i16]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        lock.write_all(&buf).context("stdout write")?;
        lock.flush().context("stdout flush")?;
        Ok(())
    }
}

/// Writes raw S16LE PCM into a named pipe. The pipe (FIFO) must already exist
/// — create one with `mkfifo`. We open it on first `write` and reopen on EPIPE
/// so a consumer can detach and reattach without restarting the stream.
pub struct PipeSink {
    path: String,
    inner: Mutex<PipeState>,
}

#[derive(Default)]
struct PipeState {
    file: Option<std::fs::File>,
    fmt: Option<PcmFormat>,
}

impl PipeSink {
    pub fn new(path: String) -> Self {
        Self {
            path,
            inner: Mutex::new(PipeState::default()),
        }
    }

    fn open(&self) -> Result<std::fs::File> {
        OpenOptions::new()
            .write(true)
            .open(&self.path)
            .with_context(|| format!("open pipe {}", self.path))
    }
}

impl AudioSink for PipeSink {
    fn set_format(&self, fmt: PcmFormat) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        if g.fmt
            .map_or(true, |old| old.sample_rate != fmt.sample_rate || old.channels != fmt.channels)
        {
            tracing::info!(
                "stream/pipe {}: format {} Hz × {} ch (S16LE)",
                self.path,
                fmt.sample_rate,
                fmt.channels
            );
            g.fmt = Some(fmt);
        }
        Ok(())
    }

    fn write(&self, samples: &[i16]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        let mut g = self.inner.lock().unwrap();
        if g.file.is_none() {
            g.file = Some(self.open()?);
        }
        // Retry once on broken-pipe to handle reader detach/reattach.
        let mut tries = 0;
        loop {
            let file = g.file.as_mut().unwrap();
            match file.write_all(&buf) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe && tries == 0 => {
                    tracing::warn!("stream/pipe {}: broken pipe, reopening", self.path);
                    g.file = Some(self.open()?);
                    tries += 1;
                    continue;
                }
                Err(e) => return Err(e).with_context(|| format!("write pipe {}", self.path)),
            }
        }
    }

    fn close(&self) {
        self.inner.lock().unwrap().file = None;
    }
}
