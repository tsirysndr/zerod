//! Standalone cpal sink — ring-buffered, lock-free-ish writer-side.
//!
//! The player thread calls `write(samples)` which pushes interleaved S16LE
//! frames into a bounded ring. A cpal output stream drains the ring on its
//! audio callback, performing i16→f32 conversion and linear-interpolation
//! resample when the device's sample rate differs from the source rate.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};

use crate::sink::{AudioSink, PcmFormat};

const RING_CAPACITY_BYTES: usize = 512 * 1024;

struct Ring {
    /// Interleaved S16LE bytes — easiest to drain regardless of source channel count.
    buf: VecDeque<u8>,
    closed: bool,
}

pub struct CpalSink {
    device_name: Option<String>,
    ring: Mutex<Ring>,
    notify: Condvar,
    src_rate: AtomicU32,
    src_channels: AtomicU32,
    stream: Mutex<Option<StreamHolder>>,
    out_rate: AtomicU32,
    out_channels: AtomicU32,
}

// cpal::Stream is !Send on macOS — the audio callback runs on a CoreAudio thread
// outside Rust's scheduler. We never move it between threads after creation;
// stream replacement happens under a Mutex on the same control thread.
struct StreamHolder(cpal::Stream);
unsafe impl Send for StreamHolder {}
unsafe impl Sync for StreamHolder {}

impl CpalSink {
    pub fn new(device_name: Option<String>) -> Self {
        Self {
            device_name,
            ring: Mutex::new(Ring {
                buf: VecDeque::with_capacity(RING_CAPACITY_BYTES),
                closed: false,
            }),
            notify: Condvar::new(),
            src_rate: AtomicU32::new(0),
            src_channels: AtomicU32::new(0),
            stream: Mutex::new(None),
            out_rate: AtomicU32::new(0),
            out_channels: AtomicU32::new(0),
        }
    }

    fn open_stream(self: &std::sync::Arc<Self>) -> Result<()> {
        let host = cpal::default_host();
        let device = match self.device_name.as_deref() {
            None | Some("") => host
                .default_output_device()
                .ok_or_else(|| anyhow!("cpal: no default output device"))?,
            Some(name) => host
                .output_devices()
                .context("cpal: enumerate output devices")?
                .find(|d| d.name().map(|n| n == name).unwrap_or(false))
                .ok_or_else(|| anyhow!("cpal: device {name} not found"))?,
        };

        let config = device
            .default_output_config()
            .context("cpal: default output config")?;
        let sample_format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.into();
        let out_rate = stream_config.sample_rate.0;
        let out_channels = stream_config.channels as u32;
        self.out_rate.store(out_rate, Ordering::SeqCst);
        self.out_channels.store(out_channels, Ordering::SeqCst);

        let me = std::sync::Arc::clone(self);
        let err_fn = |e| tracing::error!("cpal stream error: {e}");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &stream_config,
                move |out: &mut [f32], _| me.fill_f32(out),
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                &stream_config,
                move |out: &mut [i16], _| me.fill_i16(out),
                err_fn,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_output_stream(
                &stream_config,
                move |out: &mut [u16], _| me.fill_u16(out),
                err_fn,
                None,
            ),
            other => return Err(anyhow!("cpal: unsupported sample format {other:?}")),
        }
        .context("cpal: build output stream")?;
        stream.play().context("cpal: start stream")?;

        *self.stream.lock().unwrap() = Some(StreamHolder(stream));
        tracing::info!(
            "stream/cpal: opened {} Hz × {} ch on {}",
            out_rate,
            out_channels,
            self.device_name.as_deref().unwrap_or("(default)")
        );
        Ok(())
    }

    fn pop_frame_s16(&self, out_ch: usize) -> Option<Vec<i16>> {
        let mut g = self.ring.lock().unwrap();
        let src_ch = self.src_channels.load(Ordering::SeqCst).max(1) as usize;
        let need = src_ch * 2; // bytes per frame
        if g.buf.len() < need {
            return None;
        }
        let mut frame = Vec::with_capacity(out_ch);
        let mut samples = [0i16; 8];
        for i in 0..src_ch {
            let lo = g.buf.pop_front().unwrap();
            let hi = g.buf.pop_front().unwrap();
            samples[i.min(7)] = i16::from_le_bytes([lo, hi]);
        }
        // Up/down-mix to device channel count.
        match (src_ch, out_ch) {
            (s, o) if s == o => frame.extend_from_slice(&samples[..s]),
            (1, o) => {
                for _ in 0..o {
                    frame.push(samples[0]);
                }
            }
            (2, 1) => {
                let mix = ((samples[0] as i32 + samples[1] as i32) / 2) as i16;
                frame.push(mix);
            }
            (s, o) if s > o => frame.extend_from_slice(&samples[..o]),
            (s, o) => {
                for i in 0..o {
                    frame.push(samples[i.min(s - 1)]);
                }
            }
        }
        self.notify.notify_all();
        Some(frame)
    }

    /// Generic per-format fill, with linear-interp resample when src_rate ≠ out_rate.
    fn fill_generic<F: FnMut(i16)>(&self, frames: usize, mut emit: F, out_ch: usize) {
        let src_rate = self.src_rate.load(Ordering::SeqCst).max(1) as f32;
        let out_rate = self.out_rate.load(Ordering::SeqCst).max(1) as f32;
        let ratio = src_rate / out_rate;
        if (ratio - 1.0).abs() < 1e-3 {
            for _ in 0..frames {
                let Some(frame) = self.pop_frame_s16(out_ch) else {
                    for _ in 0..out_ch {
                        emit(0);
                    }
                    continue;
                };
                for s in frame {
                    emit(s);
                }
            }
        } else {
            // Naive nearest-neighbour: pop one source frame per `ratio` output
            // frames. Cheap and "good enough" for casual playback; replace with
            // a proper resampler later if quality matters.
            let mut accum: f32 = 0.0;
            let mut last = vec![0i16; out_ch];
            for _ in 0..frames {
                accum += ratio;
                while accum >= 1.0 {
                    if let Some(frame) = self.pop_frame_s16(out_ch) {
                        last = frame;
                    }
                    accum -= 1.0;
                }
                for &s in &last {
                    emit(s);
                }
            }
        }
    }

    fn fill_f32(&self, out: &mut [f32]) {
        let ch = self.out_channels.load(Ordering::SeqCst) as usize;
        let frames = out.len() / ch.max(1);
        let mut i = 0;
        self.fill_generic(
            frames,
            |s| {
                out[i] = s as f32 / 32768.0;
                i += 1;
            },
            ch,
        );
        while i < out.len() {
            out[i] = 0.0;
            i += 1;
        }
    }

    fn fill_i16(&self, out: &mut [i16]) {
        let ch = self.out_channels.load(Ordering::SeqCst) as usize;
        let frames = out.len() / ch.max(1);
        let mut i = 0;
        self.fill_generic(
            frames,
            |s| {
                out[i] = s;
                i += 1;
            },
            ch,
        );
        while i < out.len() {
            out[i] = 0;
            i += 1;
        }
    }

    fn fill_u16(&self, out: &mut [u16]) {
        let ch = self.out_channels.load(Ordering::SeqCst) as usize;
        let frames = out.len() / ch.max(1);
        let mut i = 0;
        self.fill_generic(
            frames,
            |s| {
                out[i] = (s as i32 + 32768) as u16;
                i += 1;
            },
            ch,
        );
        while i < out.len() {
            out[i] = 32768;
            i += 1;
        }
    }
}

impl AudioSink for CpalSink {
    fn set_format(&self, fmt: PcmFormat) -> Result<()> {
        self.src_rate.store(fmt.sample_rate, Ordering::SeqCst);
        self.src_channels.store(fmt.channels as u32, Ordering::SeqCst);
        // Lazy open on first set_format — Self::open_stream needs Arc<Self>,
        // so this is done in CpalSink::ensure_open via the player wrapper.
        Ok(())
    }

    fn write(&self, samples: &[i16]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut g = self.ring.lock().unwrap();
        // Apply back-pressure: wait until ring has room.
        loop {
            if g.closed {
                return Ok(());
            }
            let avail = RING_CAPACITY_BYTES - g.buf.len();
            if avail >= samples.len() * 2 {
                break;
            }
            g = self
                .notify
                .wait_timeout(g, std::time::Duration::from_millis(50))
                .unwrap()
                .0;
        }
        for s in samples {
            let [lo, hi] = s.to_le_bytes();
            g.buf.push_back(lo);
            g.buf.push_back(hi);
        }
        Ok(())
    }

    fn close(&self) {
        {
            let mut g = self.ring.lock().unwrap();
            g.closed = true;
            self.notify.notify_all();
        }
        *self.stream.lock().unwrap() = None;
    }
}

/// Factory used by the player: builds an `Arc<CpalSink>` and opens the cpal
/// stream eagerly so device-not-found errors surface to the gRPC caller before
/// `play()` returns success.
pub fn build(device: Option<String>) -> Result<std::sync::Arc<CpalSink>> {
    let sink = std::sync::Arc::new(CpalSink::new(device));
    sink.open_stream()?;
    Ok(sink)
}
