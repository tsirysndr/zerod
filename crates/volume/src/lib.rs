//! System volume control via ALSA mixer. Linux-only — non-Linux returns
//! `Err("volume: linux only")` so the gRPC layer still compiles cross-platform.
//!
//! On modern Linux audio stacks (PipeWire, PulseAudio) the ALSA mixer is
//! also exposed through ALSA-mixer emulation, so `Master` / `PCM` controls
//! work uniformly. For per-sink volume on PipeWire (e.g. raise the Bluetooth
//! speaker without touching HDMI) you'd need a native PipeWire client —
//! intentionally out of scope here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerInfo {
    pub card: String,
    pub control: String,
    pub index: u32,
    pub has_volume: bool,
    pub has_switch: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct VolumeStatus {
    pub volume_percent: u32,
    pub muted: bool,
}

/// Server-side selector. The proto-side `MixerSelector` maps onto this.
#[derive(Debug, Clone)]
pub struct Selector {
    pub card: String,
    pub control: String,
    pub index: u32,
}

impl Selector {
    pub fn new(card: Option<&str>, control: Option<&str>, index: u32) -> Self {
        Self {
            card: card.filter(|s| !s.is_empty()).unwrap_or("default").to_string(),
            control: control.filter(|s| !s.is_empty()).unwrap_or("Master").to_string(),
            index,
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{MixerInfo, Selector, VolumeStatus};
    use alsa::mixer::{Mixer, Selem, SelemChannelId, SelemId};
    use anyhow::{anyhow, Context, Result};

    fn open(card: &str) -> Result<Mixer> {
        Mixer::new(card, false).with_context(|| format!("open ALSA mixer {card}"))
    }

    fn find_selem<'a>(mixer: &'a Mixer, sel: &Selector) -> Result<Selem<'a>> {
        let id = SelemId::new(&sel.control, sel.index);
        // Try the requested control first.
        if let Some(s) = mixer.find_selem(&id) {
            return Ok(s);
        }
        // Common fallback: a card may expose "PCM" instead of "Master".
        if sel.control == "Master" {
            let fallback = SelemId::new("PCM", sel.index);
            if let Some(s) = mixer.find_selem(&fallback) {
                tracing::info!(
                    "volume: {} selem 'Master' not found on card {} — falling back to 'PCM'",
                    sel.index,
                    sel.card
                );
                return Ok(s);
            }
        }
        Err(anyhow!(
            "ALSA selem '{}' (index {}) not found on card '{}'",
            sel.control,
            sel.index,
            sel.card
        ))
    }

    fn first_channel_with_volume(selem: &Selem) -> SelemChannelId {
        // `alsa::mixer::SelemChannelId::mono()` returns FrontLeft, so iterating
        // FrontLeft → FrontRight covers mono cards too.
        for ch in [SelemChannelId::FrontLeft, SelemChannelId::FrontRight] {
            if selem.has_playback_channel(ch) {
                return ch;
            }
        }
        SelemChannelId::Unknown
    }

    pub fn list_mixers(card: Option<&str>) -> Result<Vec<MixerInfo>> {
        let card = card.filter(|s| !s.is_empty()).unwrap_or("default");
        let mixer = open(card)?;
        let mut out = Vec::new();
        for elem in mixer.iter() {
            let Some(selem) = Selem::new(elem) else { continue };
            let id = selem.get_id();
            let name = id
                .get_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|_| String::from("<unknown>"));
            out.push(MixerInfo {
                card: card.to_string(),
                control: name,
                index: id.get_index(),
                has_volume: selem.has_playback_volume(),
                has_switch: selem.has_playback_switch(),
            });
        }
        Ok(out)
    }

    pub fn get(sel: &Selector) -> Result<VolumeStatus> {
        let mixer = open(&sel.card)?;
        let selem = find_selem(&mixer, sel)?;
        let (min, max) = selem.get_playback_volume_range();
        let span = (max - min).max(1);
        let ch = first_channel_with_volume(&selem);
        let raw = if selem.has_playback_volume() {
            selem.get_playback_volume(ch).context("get_playback_volume")?
        } else {
            max
        };
        let pct = (((raw - min) * 100) / span).clamp(0, 100) as u32;
        let muted = if selem.has_playback_switch() {
            selem.get_playback_switch(ch).context("get_playback_switch")? == 0
        } else {
            false
        };
        Ok(VolumeStatus {
            volume_percent: pct,
            muted,
        })
    }

    pub fn set_volume(sel: &Selector, percent: u32) -> Result<()> {
        let percent = percent.min(100) as i64;
        let mixer = open(&sel.card)?;
        let selem = find_selem(&mixer, sel)?;
        if !selem.has_playback_volume() {
            return Err(anyhow!(
                "ALSA selem '{}' has no playback volume",
                sel.control
            ));
        }
        let (min, max) = selem.get_playback_volume_range();
        let target = min + (percent * (max - min)) / 100;
        selem
            .set_playback_volume_all(target)
            .with_context(|| format!("set_playback_volume_all({target})"))?;
        publish_volume(sel);
        Ok(())
    }

    pub fn set_mute(sel: &Selector, muted: bool) -> Result<()> {
        let mixer = open(&sel.card)?;
        let selem = find_selem(&mixer, sel)?;
        if !selem.has_playback_switch() {
            return Err(anyhow!(
                "ALSA selem '{}' has no playback switch (mute)",
                sel.control
            ));
        }
        selem
            .set_playback_switch_all(if muted { 0 } else { 1 })
            .with_context(|| format!("set_playback_switch_all(muted={muted})"))?;
        publish_volume(sel);
        Ok(())
    }

    fn publish_volume(sel: &Selector) {
        if let Ok(st) = get(sel) {
            zerod_events::publish(zerod_events::Event::VolumeChanged {
                card: sel.card.clone(),
                control: sel.control.clone(),
                volume_percent: st.volume_percent,
                muted: st.muted,
            });
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{MixerInfo, Selector, VolumeStatus};
    use anyhow::{bail, Result};

    pub fn list_mixers(_card: Option<&str>) -> Result<Vec<MixerInfo>> {
        bail!("volume: linux only")
    }
    pub fn get(_sel: &Selector) -> Result<VolumeStatus> {
        bail!("volume: linux only")
    }
    pub fn set_volume(_sel: &Selector, _percent: u32) -> Result<()> {
        bail!("volume: linux only")
    }
    pub fn set_mute(_sel: &Selector, _muted: bool) -> Result<()> {
        bail!("volume: linux only")
    }
}

pub use imp::{get, list_mixers, set_mute, set_volume};
