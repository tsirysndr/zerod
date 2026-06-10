//! Singleton player state machine. One stream at a time.

use anyhow::{anyhow, Result};
use bytes::Bytes;
use once_cell::sync::Lazy;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use crate::cpal_sink;
use crate::decoder::decode_segment;
use crate::demux;
use crate::fetcher::{self, SegmentCache};
use crate::manifest::{self, ManifestKind};
use crate::output::{PipeSink, StdoutSink};
use crate::sink::{AudioOutput, AudioSink, PcmFormat};

static RT: Lazy<Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("zerod-stream")
        .build()
        .expect("stream: build tokio runtime")
});

static PLAYER: Lazy<Mutex<Option<Arc<Player>>>> = Lazy::new(|| Mutex::new(None));

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Stopped = 0,
    Buffering = 1,
    Playing = 2,
    Paused = 3,
    Errored = 4,
}

pub struct PlayConfig {
    pub url: String,
    pub output: AudioOutput,
}

pub struct Status {
    pub state: PlayerState,
    pub url: String,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub is_live: bool,
    pub error: Option<String>,
    pub output: AudioOutput,
}

struct Player {
    url: String,
    output: AudioOutput,
    sink: Arc<dyn AudioSink>,
    state: AtomicU8,
    paused: AtomicBool,
    stop_flag: Arc<AtomicBool>,
    position_ms: AtomicI64,
    duration_ms: AtomicI64,
    is_live: AtomicBool,
    task: Mutex<Option<JoinHandle<()>>>,
    last_error: Mutex<Option<String>>,
}

impl Player {
    fn set_state(&self, s: PlayerState) {
        self.state.store(s as u8, Ordering::SeqCst);
    }

    fn state(&self) -> PlayerState {
        match self.state.load(Ordering::SeqCst) {
            0 => PlayerState::Stopped,
            1 => PlayerState::Buffering,
            2 => PlayerState::Playing,
            3 => PlayerState::Paused,
            _ => PlayerState::Errored,
        }
    }

    fn record_error(&self, msg: String) {
        tracing::error!("stream: {msg}");
        *self.last_error.lock().unwrap() = Some(msg);
        self.set_state(PlayerState::Errored);
    }

    fn cancel(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.sink.close();
        if let Some(h) = self.task.lock().unwrap().take() {
            h.abort();
        }
    }
}

fn build_sink(out: &AudioOutput) -> Result<Arc<dyn AudioSink>> {
    match out {
        AudioOutput::Cpal { device } => {
            let sink = cpal_sink::build(device.clone())?;
            Ok(sink as Arc<dyn AudioSink>)
        }
        AudioOutput::Stdout => Ok(Arc::new(StdoutSink::new()) as Arc<dyn AudioSink>),
        AudioOutput::Pipe { path } => {
            Ok(Arc::new(PipeSink::new(path.clone())) as Arc<dyn AudioSink>)
        }
    }
}

async fn run_player(player: Arc<Player>) {
    if let Err(e) = run_player_inner(&player).await {
        player.record_error(format!("{e:#}"));
    } else {
        player.set_state(PlayerState::Stopped);
    }
    player.sink.close();
}

async fn run_player_inner(player: &Arc<Player>) -> Result<()> {
    let client = Arc::new(
        reqwest::Client::builder()
            .user_agent("zerod-stream/0.1")
            .timeout(Duration::from_secs(20))
            .build()?,
    );
    player.set_state(PlayerState::Buffering);

    let url = player.url.clone();
    let kind = manifest::is_hls_or_dash_url(&url)
        .ok_or_else(|| anyhow!("URL does not look like HLS or DASH: {url}"))?;
    let snap = match manifest::fetch_and_parse(&client, &url, kind).await {
        Ok(s) => s,
        Err(e) if e.to_string().contains("re-fetch variant") => {
            let s = e.to_string();
            let variant = s
                .rsplit(' ')
                .next()
                .ok_or_else(|| anyhow!("variant url parse: {e}"))?
                .to_string();
            manifest::fetch_and_parse(&client, &variant, ManifestKind::Hls).await?
        }
        Err(e) => return Err(e),
    };
    player.is_live.store(snap.is_live, Ordering::SeqCst);
    player.duration_ms.store(
        snap.duration.map(|d| (d * 1000.0) as i64).unwrap_or(-1),
        Ordering::SeqCst,
    );

    let cache = Arc::new(SegmentCache::default());
    let mut init_bytes: Option<Bytes> = None;
    let mut current_fmt: Option<PcmFormat> = None;
    if let Some(init_url) = snap.init_url.clone() {
        let init = fetcher::fetch_bytes(&client, &init_url).await?;
        match demux::parse_init(&init) {
            Ok(h) => {
                if let (Some(sr), Some(ch)) = (h.sample_rate, h.channels) {
                    let fmt = PcmFormat {
                        sample_rate: sr,
                        channels: ch,
                    };
                    player.sink.set_format(fmt)?;
                    current_fmt = Some(fmt);
                }
            }
            Err(e) => tracing::warn!("stream: parse init failed ({e}); decoder will probe per-segment"),
        }
        init_bytes = Some(init);
    }

    let mut next_play_seq = snap
        .segments
        .first()
        .map(|s| s.seq)
        .ok_or_else(|| anyhow!("manifest has no segments"))?;
    if snap.is_live {
        let n = snap.segments.len();
        if n > 3 {
            next_play_seq = snap.segments[n - 3].seq;
        }
    }

    let initial: Vec<_> = snap
        .segments
        .iter()
        .filter(|s| s.seq >= next_play_seq)
        .take(3)
        .cloned()
        .collect();
    fetcher::prefetch(client.clone(), cache.clone(), initial).await;

    player.set_state(PlayerState::Playing);

    let known: Arc<Mutex<VecDeque<crate::manifest::SegmentRef>>> =
        Arc::new(Mutex::new(snap.segments.iter().cloned().collect()));

    let refresher = if snap.is_live {
        let client = client.clone();
        let known = known.clone();
        let cache = cache.clone();
        let stop = player.stop_flag.clone();
        let interval = snap.refresh_interval;
        let url = url.clone();
        let kind = snap.kind;
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                match manifest::fetch_and_parse(&client, &url, kind).await {
                    Ok(new_snap) => {
                        let snapshot: Vec<_> = {
                            let mut g = known.lock().unwrap();
                            let last_seen = g.back().map(|s| s.seq).unwrap_or(0);
                            for s in new_snap.segments {
                                if s.seq > last_seen {
                                    g.push_back(s);
                                }
                            }
                            while g.len() > 64 {
                                g.pop_front();
                            }
                            g.iter().rev().take(3).cloned().collect()
                        };
                        fetcher::prefetch(client.clone(), cache.clone(), snapshot).await;
                    }
                    Err(e) => tracing::warn!("stream refresh: {e}"),
                }
            }
        }))
    } else {
        None
    };

    loop {
        if player.stop_flag.load(Ordering::SeqCst) {
            break;
        }
        if player.paused.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        let seg = {
            let g = known.lock().unwrap();
            g.iter().find(|s| s.seq == next_play_seq).cloned()
        };
        let Some(seg) = seg else {
            if snap.is_live {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            break;
        };

        let bytes = match cache.get(seg.seq).await {
            Some(b) => b,
            None => match fetcher::fetch_bytes(&client, &seg.url).await {
                Ok(b) => {
                    cache.put(seg.seq, b.clone()).await;
                    b
                }
                Err(e) => {
                    tracing::warn!("stream: fetch seg {} failed: {e}; skipping", seg.seq);
                    next_play_seq += 1;
                    continue;
                }
            },
        };

        let decoded = match decode_segment(init_bytes.as_deref(), &bytes) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("stream: decode seg {} failed: {e}; skipping", seg.seq);
                next_play_seq += 1;
                continue;
            }
        };

        let fmt = PcmFormat {
            sample_rate: decoded.sample_rate,
            channels: decoded.channels,
        };
        if current_fmt != Some(fmt) {
            player.sink.set_format(fmt)?;
            current_fmt = Some(fmt);
        }

        let ch = decoded.channels.max(1) as usize;
        let chunk_frames = decoded.sample_rate.max(1) as usize / 20; // ~50 ms
        let chunk_samples = (chunk_frames * ch).max(ch);
        for window in decoded.samples.chunks(chunk_samples) {
            if player.stop_flag.load(Ordering::SeqCst) {
                break;
            }
            while player.paused.load(Ordering::SeqCst) && !player.stop_flag.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            player.sink.write(window)?;
            let frames_pushed = window.len() / ch;
            let ms = (frames_pushed as i64 * 1000) / decoded.sample_rate.max(1) as i64;
            player.position_ms.fetch_add(ms, Ordering::SeqCst);
        }

        let upcoming: Vec<_> = {
            let g = known.lock().unwrap();
            g.iter()
                .filter(|s| s.seq > next_play_seq && s.seq <= next_play_seq + 3)
                .cloned()
                .collect()
        };
        fetcher::prefetch(client.clone(), cache.clone(), upcoming).await;

        next_play_seq += 1;
    }

    if let Some(h) = refresher {
        h.abort();
    }
    Ok(())
}

fn current() -> Option<Arc<Player>> {
    PLAYER.lock().unwrap().clone()
}

pub fn play(cfg: PlayConfig) -> Result<()> {
    if manifest::is_hls_or_dash_url(&cfg.url).is_none() {
        return Err(anyhow!("not an HLS or DASH URL: {}", cfg.url));
    }
    let sink = build_sink(&cfg.output)?;
    let player = Arc::new(Player {
        url: cfg.url,
        output: cfg.output,
        sink,
        state: AtomicU8::new(PlayerState::Stopped as u8),
        paused: AtomicBool::new(false),
        stop_flag: Arc::new(AtomicBool::new(false)),
        position_ms: AtomicI64::new(0),
        duration_ms: AtomicI64::new(-1),
        is_live: AtomicBool::new(false),
        task: Mutex::new(None),
        last_error: Mutex::new(None),
    });
    let runner = player.clone();
    let task = RT.spawn(async move { run_player(runner).await });
    *player.task.lock().unwrap() = Some(task);

    let mut g = PLAYER.lock().unwrap();
    if let Some(old) = g.take() {
        old.cancel();
    }
    *g = Some(player);
    Ok(())
}

pub fn pause() -> bool {
    if let Some(p) = current() {
        p.paused.store(true, Ordering::SeqCst);
        p.set_state(PlayerState::Paused);
        true
    } else {
        false
    }
}

pub fn resume() -> bool {
    if let Some(p) = current() {
        p.paused.store(false, Ordering::SeqCst);
        p.set_state(PlayerState::Playing);
        true
    } else {
        false
    }
}

pub fn stop() -> bool {
    let mut g = PLAYER.lock().unwrap();
    if let Some(p) = g.take() {
        p.cancel();
        p.set_state(PlayerState::Stopped);
        true
    } else {
        false
    }
}

pub fn status() -> Status {
    if let Some(p) = current() {
        Status {
            state: p.state(),
            url: p.url.clone(),
            position_ms: p.position_ms.load(Ordering::SeqCst),
            duration_ms: p.duration_ms.load(Ordering::SeqCst),
            is_live: p.is_live.load(Ordering::SeqCst),
            error: p.last_error.lock().unwrap().clone(),
            output: p.output.clone(),
        }
    } else {
        Status {
            state: PlayerState::Stopped,
            url: String::new(),
            position_ms: 0,
            duration_ms: -1,
            is_live: false,
            error: None,
            output: AudioOutput::Cpal { device: None },
        }
    }
}
