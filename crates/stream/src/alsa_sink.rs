//! Direct libasound sink (Linux only).
//!
//! We use `alsa::pcm::PCM` with `Access::RWInterleaved` + `snd_pcm_writei`,
//! exactly the way `aplay` does. That avoids cpal-alsa's mmap mode, which
//! crashes inside libasound's pulse plugin on Raspberry Pi OS (`build_output_stream`
//! → `snd_pcm_mmap_begin` SIGSEGV).
//!
//! Architecture: a dedicated OS thread owns the PCM handle and drains a
//! bounded mpsc channel. The player's tokio task pushes samples onto the
//! channel without ever touching libasound, so an ALSA stall can't wedge a
//! tokio worker.

use anyhow::{Context, Result};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::sink::{AudioSink, PcmFormat};

const CHANNEL_CAPACITY: usize = 64;
const PERIOD_FRAMES: alsa::pcm::Frames = 1024;   // ~23 ms @ 44.1 kHz
const BUFFER_FRAMES: alsa::pcm::Frames = 8192;   // ~185 ms @ 44.1 kHz

enum Msg {
    SetFormat(PcmFormat),
    Write(Vec<i16>),
    Close,
}

pub struct AlsaSink {
    device_name: String,
    tx: Mutex<Option<SyncSender<Msg>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl AlsaSink {
    fn new(device: Option<String>) -> Arc<Self> {
        let device_name = device
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".to_string());
        let (tx, rx) = sync_channel::<Msg>(CHANNEL_CAPACITY);
        let device_for_thread = device_name.clone();
        let handle = std::thread::Builder::new()
            .name("zerod-alsa".into())
            .spawn(move || run_writer(device_for_thread, rx))
            .expect("spawn alsa writer thread");
        Arc::new(Self {
            device_name,
            tx: Mutex::new(Some(tx)),
            handle: Mutex::new(Some(handle)),
        })
    }
}

impl AudioSink for AlsaSink {
    fn set_format(&self, fmt: PcmFormat) -> Result<()> {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            // Drop the format change if the writer thread is wedged — we'd
            // rather emit a glitch than block the tokio worker indefinitely.
            let _ = tx.try_send(Msg::SetFormat(fmt));
        }
        Ok(())
    }

    fn write(&self, samples: &[i16]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let Some(tx) = self.tx.lock().unwrap().clone() else {
            return Ok(());
        };
        // Bounded blocking send: applies back-pressure to the decode loop
        // when the alsa thread can't keep up. At 50ms chunks × 64 capacity
        // that's ~3s of buffer before we block.
        tx.send(Msg::Write(samples.to_vec()))
            .context("alsa: writer thread is gone")?;
        Ok(())
    }

    fn close(&self) {
        let tx = self.tx.lock().unwrap().take();
        if let Some(tx) = tx {
            let _ = tx.send(Msg::Close);
        }
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

impl Drop for AlsaSink {
    fn drop(&mut self) {
        self.close();
    }
}

fn open_pcm(device: &str, fmt: PcmFormat) -> Result<alsa::PCM> {
    let pcm = alsa::PCM::new(device, alsa::Direction::Playback, false)
        .with_context(|| format!("alsa: open PCM {device}"))?;
    {
        let hwp = alsa::pcm::HwParams::any(&pcm).context("alsa: HwParams::any")?;
        hwp.set_access(alsa::pcm::Access::RWInterleaved)
            .context("alsa: set_access")?;
        hwp.set_format(alsa::pcm::Format::s16())
            .context("alsa: set_format")?;
        hwp.set_channels(fmt.channels as u32)
            .with_context(|| format!("alsa: set_channels({})", fmt.channels))?;
        hwp.set_rate(fmt.sample_rate, alsa::ValueOr::Nearest)
            .with_context(|| format!("alsa: set_rate({})", fmt.sample_rate))?;
        let _ = hwp.set_buffer_size_near(BUFFER_FRAMES);
        let _ = hwp.set_period_size_near(PERIOD_FRAMES, alsa::ValueOr::Nearest);
        pcm.hw_params(&hwp).context("alsa: hw_params apply")?;
    }
    pcm.prepare().context("alsa: prepare")?;
    Ok(pcm)
}

fn run_writer(device: String, rx: Receiver<Msg>) {
    let mut pcm: Option<alsa::PCM> = None;
    let mut current_fmt: Option<PcmFormat> = None;
    tracing::info!("stream/alsa: writer thread started (device={device})");

    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::SetFormat(fmt) => {
                if current_fmt == Some(fmt) {
                    continue;
                }
                if let Some(old) = pcm.take() {
                    let _ = old.drain();
                }
                match open_pcm(&device, fmt) {
                    Ok(new) => {
                        tracing::info!(
                            "stream/alsa: opened {} at {} Hz × {} ch",
                            device,
                            fmt.sample_rate,
                            fmt.channels
                        );
                        pcm = Some(new);
                        current_fmt = Some(fmt);
                    }
                    Err(e) => {
                        tracing::error!("stream/alsa: open failed: {e:#}");
                        // Keep `pcm` as None — subsequent writes are dropped
                        // until a successful set_format reopens.
                    }
                }
            }
            Msg::Write(samples) => {
                let Some(p) = pcm.as_ref() else { continue };
                let Some(fmt) = current_fmt else { continue };
                let ch = (fmt.channels as usize).max(1);
                let io = match p.io_i16() {
                    Ok(io) => io,
                    Err(e) => {
                        tracing::error!("stream/alsa: io_i16: {e}");
                        continue;
                    }
                };
                let mut offset = 0usize;
                while offset < samples.len() {
                    let frame_start = offset / ch;
                    let chunk = &samples[offset..];
                    match io.writei(chunk) {
                        Ok(n) => {
                            offset += n * ch;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "stream/alsa: writei error at frame {frame_start}: {e}, recovering"
                            );
                            if let Err(re) = p.try_recover(e, true) {
                                tracing::error!("stream/alsa: recover failed: {re}");
                                break;
                            }
                        }
                    }
                }
            }
            Msg::Close => break,
        }
    }

    if let Some(p) = pcm {
        let _ = p.drain();
    }
    tracing::info!("stream/alsa: writer thread exiting");
}

/// Factory used by the player. Spawns the writer thread and returns the sink.
pub fn build(device: Option<String>) -> Result<Arc<AlsaSink>> {
    Ok(AlsaSink::new(device))
}
