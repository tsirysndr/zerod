//! Standalone cpal sink — lock-free SPSC ring + linear-interp resampler.
//!
//! Writer (player thread) pushes interleaved S16LE samples into an rtrb
//! ring via `AudioSink::write`. The cpal output callback owns the consumer
//! half and does, per buffer:
//!   1. Channel remap (mono ↔ stereo ↔ N) to the device layout.
//!   2. Linear interpolation between adjacent source frames when the source
//!      rate differs from the device rate — the common case is librespot
//!      44.1k → macOS default 48k, where zero-order hold produces audible
//!      aliasing.
//!   3. Conversion to the device's native sample format.
//! No locks, allocations, or syscalls on the audio thread.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::sink::{AudioSink, PcmFormat};

const RING_CAPACITY_SAMPLES: usize = 512 * 1024;
const MAX_CHANNELS: usize = 8;

// cpal::Stream is !Send on macOS — the audio callback runs on a CoreAudio
// thread outside Rust's scheduler. We never move it between threads after
// creation; stream replacement happens under a Mutex on the same control
// thread.
struct StreamHolder(cpal::Stream);
unsafe impl Send for StreamHolder {}
unsafe impl Sync for StreamHolder {}

pub struct CpalSink {
    device_name: Option<String>,
    producer: Mutex<Option<Producer<i16>>>,
    stream: Mutex<Option<StreamHolder>>,
    src_rate: AtomicU32,
    src_channels: AtomicU32,
    out_rate: AtomicU32,
    out_channels: AtomicU32,
}

struct Resampler {
    prev: [i16; MAX_CHANNELS],
    next: [i16; MAX_CHANNELS],
    frac: f32,
    primed: bool,
}

impl Resampler {
    fn new() -> Self {
        Self {
            prev: [0; MAX_CHANNELS],
            next: [0; MAX_CHANNELS],
            frac: 0.0,
            primed: false,
        }
    }

    #[inline]
    fn pop_frame(consumer: &mut Consumer<i16>, src_ch: usize, dst: &mut [i16; MAX_CHANNELS]) {
        for slot in dst.iter_mut().take(src_ch) {
            if let Ok(s) = consumer.pop() {
                *slot = s;
            }
        }
    }

    fn fill<F: FnMut(i16)>(
        &mut self,
        consumer: &mut Consumer<i16>,
        src_rate: u32,
        src_ch: usize,
        out_ch: usize,
        ratio: f32,
        frames: usize,
        mut emit: F,
    ) {
        // Pre-buffer ~1.5s of source frames before unleashing the resampler.
        // HLS playback has a producer-side gap between segments (decode +
        // occasional network fetch) that can spike past a smaller buffer
        // under network jitter, causing random clicks. 1.5s is comfortably
        // above the worst case while staying well under ring capacity.
        if !self.primed {
            let prebuf_frames = (src_rate as usize * 3 / 2).max(2);
            let need = (prebuf_frames * src_ch).min(RING_CAPACITY_SAMPLES / 2);
            if consumer.slots() < need {
                for _ in 0..frames * out_ch {
                    emit(0);
                }
                return;
            }
            Self::pop_frame(consumer, src_ch, &mut self.prev);
            Self::pop_frame(consumer, src_ch, &mut self.next);
            self.frac = 0.0;
            self.primed = true;
        }

        for frame_idx in 0..frames {
            // Advance source frames as needed.
            while self.frac >= 1.0 {
                if consumer.slots() < src_ch {
                    // Underrun mid-buffer: fill the remaining output with
                    // silence inline (a clean fade) and re-prime next
                    // callback so we don't resume from a near-empty ring.
                    for _ in frame_idx..frames {
                        for _ in 0..out_ch {
                            emit(0);
                        }
                    }
                    self.primed = false;
                    self.frac = 0.0;
                    return;
                }
                self.prev = self.next;
                Self::pop_frame(consumer, src_ch, &mut self.next);
                self.frac -= 1.0;
            }

            let f = self.frac;
            for ch in 0..out_ch {
                let (a, b) = remap(src_ch, out_ch, ch, &self.prev, &self.next);
                let s = a as f32 + (b as f32 - a as f32) * f;
                emit(s.clamp(-32768.0, 32767.0) as i16);
            }
            self.frac += ratio;
        }
    }
}

#[inline]
fn remap(
    src_ch: usize,
    out_ch: usize,
    ch: usize,
    prev: &[i16; MAX_CHANNELS],
    next: &[i16; MAX_CHANNELS],
) -> (i16, i16) {
    if src_ch == out_ch {
        return (prev[ch], next[ch]);
    }
    if src_ch == 1 {
        return (prev[0], next[0]);
    }
    if out_ch == 1 {
        let mut p: i32 = 0;
        let mut n: i32 = 0;
        for i in 0..src_ch {
            p += prev[i] as i32;
            n += next[i] as i32;
        }
        let s = src_ch as i32;
        return ((p / s) as i16, (n / s) as i16);
    }
    let i = ch.min(src_ch - 1);
    (prev[i], next[i])
}

impl CpalSink {
    pub fn new(device_name: Option<String>) -> Self {
        Self {
            device_name,
            producer: Mutex::new(None),
            stream: Mutex::new(None),
            src_rate: AtomicU32::new(0),
            src_channels: AtomicU32::new(0),
            out_rate: AtomicU32::new(0),
            out_channels: AtomicU32::new(0),
        }
    }

    fn open_stream(self: &Arc<Self>) -> Result<()> {
        tracing::info!(
            "stream/cpal: open_stream begin (device={:?})",
            self.device_name
        );

        let host = cpal::default_host();
        tracing::info!("stream/cpal: host id = {:?}", host.id());

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
        let device_name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());
        tracing::info!("stream/cpal: selected device = {}", device_name);

        let config = device
            .default_output_config()
            .context("cpal: default output config")?;
        let sample_format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.into();
        let out_rate = stream_config.sample_rate.0;
        let out_channels = stream_config.channels as u32;
        if out_channels as usize > MAX_CHANNELS {
            return Err(anyhow!(
                "cpal: device requests {out_channels} channels, max is {MAX_CHANNELS}"
            ));
        }
        tracing::info!(
            "stream/cpal: default config = {} Hz × {} ch, sample_format = {:?}",
            out_rate,
            out_channels,
            sample_format
        );
        self.out_rate.store(out_rate, Ordering::SeqCst);
        self.out_channels.store(out_channels, Ordering::SeqCst);

        let (producer, consumer) = RingBuffer::<i16>::new(RING_CAPACITY_SAMPLES);
        *self.producer.lock().unwrap() = Some(producer);

        let err_fn = |e| tracing::error!("cpal stream error: {e}");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let mut state = Resampler::new();
                let mut consumer = consumer;
                let shared = Arc::clone(self);
                device.build_output_stream(
                    &stream_config,
                    move |out: &mut [f32], _| {
                        let out_len = out.len();
                        let src_rate = shared.src_rate.load(Ordering::Relaxed).max(1);
                        let src_ch = (shared.src_channels.load(Ordering::Relaxed).max(1) as usize)
                            .min(MAX_CHANNELS);
                        let out_ch = shared.out_channels.load(Ordering::Relaxed).max(1) as usize;
                        let ratio = src_rate as f32
                            / shared.out_rate.load(Ordering::Relaxed).max(1) as f32;
                        let frames = out_len / out_ch.max(1);
                        let mut i = 0;
                        state.fill(&mut consumer, src_rate, src_ch, out_ch, ratio, frames, |s| {
                            if i < out_len {
                                out[i] = s as f32 / 32768.0;
                                i += 1;
                            }
                        });
                        while i < out_len {
                            out[i] = 0.0;
                            i += 1;
                        }
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let mut state = Resampler::new();
                let mut consumer = consumer;
                let shared = Arc::clone(self);
                device.build_output_stream(
                    &stream_config,
                    move |out: &mut [i16], _| {
                        let out_len = out.len();
                        let src_rate = shared.src_rate.load(Ordering::Relaxed).max(1);
                        let src_ch = (shared.src_channels.load(Ordering::Relaxed).max(1) as usize)
                            .min(MAX_CHANNELS);
                        let out_ch = shared.out_channels.load(Ordering::Relaxed).max(1) as usize;
                        let ratio = src_rate as f32
                            / shared.out_rate.load(Ordering::Relaxed).max(1) as f32;
                        let frames = out_len / out_ch.max(1);
                        let mut i = 0;
                        state.fill(&mut consumer, src_rate, src_ch, out_ch, ratio, frames, |s| {
                            if i < out_len {
                                out[i] = s;
                                i += 1;
                            }
                        });
                        while i < out_len {
                            out[i] = 0;
                            i += 1;
                        }
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let mut state = Resampler::new();
                let mut consumer = consumer;
                let shared = Arc::clone(self);
                device.build_output_stream(
                    &stream_config,
                    move |out: &mut [u16], _| {
                        let out_len = out.len();
                        let src_rate = shared.src_rate.load(Ordering::Relaxed).max(1);
                        let src_ch = (shared.src_channels.load(Ordering::Relaxed).max(1) as usize)
                            .min(MAX_CHANNELS);
                        let out_ch = shared.out_channels.load(Ordering::Relaxed).max(1) as usize;
                        let ratio = src_rate as f32
                            / shared.out_rate.load(Ordering::Relaxed).max(1) as f32;
                        let frames = out_len / out_ch.max(1);
                        let mut i = 0;
                        state.fill(&mut consumer, src_rate, src_ch, out_ch, ratio, frames, |s| {
                            if i < out_len {
                                out[i] = (s as i32 + 32768) as u16;
                                i += 1;
                            }
                        });
                        while i < out_len {
                            out[i] = 32768;
                            i += 1;
                        }
                    },
                    err_fn,
                    None,
                )
            }
            other => return Err(anyhow!("cpal: unsupported sample format {other:?}")),
        }
        .context("cpal: build output stream")?;

        stream.play().context("cpal: start stream")?;
        *self.stream.lock().unwrap() = Some(StreamHolder(stream));

        tracing::info!(
            "stream/cpal: opened {} Hz × {} ch on {}",
            out_rate,
            out_channels,
            device_name
        );
        Ok(())
    }
}

impl AudioSink for CpalSink {
    fn set_format(&self, fmt: PcmFormat) -> Result<()> {
        self.src_rate.store(fmt.sample_rate, Ordering::SeqCst);
        self.src_channels
            .store(fmt.channels as u32, Ordering::SeqCst);
        Ok(())
    }

    fn write(&self, samples: &[i16]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut written = 0;
        while written < samples.len() {
            let mut guard = self.producer.lock().unwrap();
            let producer = match guard.as_mut() {
                Some(p) => p,
                None => return Ok(()),
            };
            let slots = producer.slots();
            if slots == 0 {
                drop(guard);
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            let n = (samples.len() - written).min(slots);
            // write_chunk gives us two contiguous slices (the ring may wrap)
            // and lets us memcpy in bulk rather than push() per sample.
            let mut chunk = producer
                .write_chunk(n)
                .expect("slots checked >= n above");
            let (a, b) = chunk.as_mut_slices();
            let split = a.len();
            a.copy_from_slice(&samples[written..written + split]);
            b.copy_from_slice(&samples[written + split..written + n]);
            chunk.commit_all();
            written += n;
        }
        Ok(())
    }

    fn close(&self) {
        *self.stream.lock().unwrap() = None;
        *self.producer.lock().unwrap() = None;
    }
}

/// Factory used by the player: builds an `Arc<CpalSink>` and opens the cpal
/// stream eagerly so device-not-found errors surface to the gRPC caller
/// before `play()` returns success.
pub fn build(device: Option<String>) -> Result<Arc<CpalSink>> {
    let sink = Arc::new(CpalSink::new(device));
    sink.open_stream()?;
    Ok(sink)
}
