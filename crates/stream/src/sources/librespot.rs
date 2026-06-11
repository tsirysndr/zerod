//! Librespot subprocess source.
//!
//! Spawns `librespot --backend pipe --format S16 --device -` so the child
//! writes raw interleaved S16LE PCM at 44.1 kHz / 2ch to stdout. A tokio
//! task reads in 8 KiB chunks, converts byte pairs to `i16` via
//! `from_le_bytes` (endian-safe across cross-compile targets), applies the
//! existing per-stream gain, and forwards into the same `AudioSink` as
//! HLS/DASH playback. Stderr is mirrored at TRACE so unsolicited
//! librespot chatter doesn't dominate logs.
//!
//! The child is held via `tokio::process::Command::kill_on_drop(true)`
//! so an `abort()` from `Player::cancel()` reliably reaps it.

use anyhow::{anyhow, Context, Result};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::LibrespotConfig;
use crate::player::{
    apply_gain_pub, build_sink_pub, global_volume, install, runtime, PlaybackSource, Player,
    PlayerState,
};
use crate::sink::PcmFormat;

pub fn spotify_start(cfg: LibrespotConfig) -> Result<()> {
    let sink = build_sink_pub(&cfg.output)?;
    let player = Arc::new(Player::new(
        format!("spotify://{}", cfg.name),
        cfg.output.clone(),
        sink,
        PlaybackSource::Spotify,
    ));
    let runner = player.clone();
    let task = runtime().spawn(async move { run_librespot(runner, cfg).await });
    install(player, task);
    Ok(())
}

pub fn spotify_stop() -> bool {
    crate::player::stop()
}

async fn run_librespot(player: Arc<Player>, cfg: LibrespotConfig) {
    if let Err(e) = run_librespot_inner(&player, cfg).await {
        player.record_error(format!("{e:#}"));
    } else {
        player.set_state(PlayerState::Stopped);
    }
    player.sink.close();
}

async fn run_librespot_inner(player: &Arc<Player>, cfg: LibrespotConfig) -> Result<()> {
    player.set_state(PlayerState::Buffering);
    // librespot --backend pipe emits 44100 / 2ch S16LE regardless of
    // source quality. Fix the sink format up front.
    let fmt = PcmFormat {
        sample_rate: 44_100,
        channels: 2,
    };
    player.sink.set_format(fmt)?;

    let mut child = spawn_librespot(&cfg)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("librespot: no stdout"))?;
    if let Some(stderr) = child.stderr.take() {
        runtime().spawn(forward_stderr(stderr));
    }

    player.set_state(PlayerState::Playing);

    let mut buf = vec![0u8; 8192];
    loop {
        if player.stop_flag.load(Ordering::SeqCst) {
            break;
        }
        let n = match stdout.read(&mut buf).await {
            Ok(0) => break, // EOF — child exited
            Ok(n) => n,
            Err(e) => return Err(anyhow!("librespot stdout read: {e}")),
        };
        let mut samples = Vec::with_capacity(n / 2);
        for pair in buf[..n].chunks_exact(2) {
            samples.push(i16::from_le_bytes([pair[0], pair[1]]));
        }
        let vol = global_volume();
        if vol < 100 {
            apply_gain_pub(&mut samples, vol);
        }
        player.sink.write(&samples)?;
    }

    let _ = child.kill().await;
    Ok(())
}

fn spawn_librespot(cfg: &LibrespotConfig) -> Result<Child> {
    let mut cmd = Command::new(&cfg.binary);
    cmd.kill_on_drop(true);
    // TODO verify these flags against the installed librespot version.
    // 0.4.x ships `--backend pipe` + `--format S16`; older builds used
    // `--backend pipe-stdout`. CI Pi smoke test should catch a regression.
    cmd.arg("--name").arg(&cfg.name);
    cmd.arg("--bitrate").arg(cfg.bitrate.to_string());
    cmd.arg("--backend").arg("pipe");
    cmd.arg("--device").arg("-");
    cmd.arg("--format").arg("S16");
    cmd.arg("--initial-volume").arg("100");
    if !cfg.cache_path.is_empty() {
        cmd.arg("--cache").arg(&cfg.cache_path);
        // Cache credentials but not raw audio chunks — disk-cheap on Pi.
        cmd.arg("--disable-audio-cache");
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.spawn()
        .with_context(|| format!("spawn librespot binary `{}`", cfg.binary))
}

async fn forward_stderr(stderr: tokio::process::ChildStderr) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!("librespot: {line}");
    }
}
