//! HLS (.m3u8) + DASH (.mpd) manifest parsing, unified into one snapshot.

use anyhow::{anyhow, Context, Result};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestKind {
    Hls,
    Dash,
}

#[derive(Debug, Clone)]
pub struct SegmentRef {
    pub seq: u64,
    pub url: String,
    pub duration: f64,
    pub container: ContainerHint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerHint {
    Fmp4,
    MpegTs,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ManifestSnapshot {
    pub kind: ManifestKind,
    pub base_url: Url,
    pub init_url: Option<String>,
    pub segments: Vec<SegmentRef>,
    pub is_live: bool,
    pub refresh_interval: Duration,
    pub duration: Option<f64>,
}

pub fn is_hls_or_dash_url(url: &str) -> Option<ManifestKind> {
    let lower = url.to_ascii_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    if path.ends_with(".m3u8") || path.ends_with(".m3u") {
        Some(ManifestKind::Hls)
    } else if path.ends_with(".mpd") {
        Some(ManifestKind::Dash)
    } else {
        None
    }
}

pub async fn fetch_and_parse(
    client: &reqwest::Client,
    url: &str,
    kind: ManifestKind,
) -> Result<ManifestSnapshot> {
    let body = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetch manifest {url}"))?
        .error_for_status()
        .with_context(|| format!("manifest http error {url}"))?
        .text()
        .await
        .with_context(|| format!("read manifest body {url}"))?;

    let base = Url::parse(url).with_context(|| format!("parse manifest url {url}"))?;
    match kind {
        ManifestKind::Hls => parse_hls(&body, base),
        ManifestKind::Dash => parse_dash(&body, base),
    }
}

fn parse_hls(body: &str, base: Url) -> Result<ManifestSnapshot> {
    let bytes = body.as_bytes();
    match m3u8_rs::parse_playlist_res(bytes) {
        Ok(m3u8_rs::Playlist::MasterPlaylist(master)) => {
            let variant = master
                .variants
                .iter()
                .max_by_key(|v| v.bandwidth)
                .ok_or_else(|| anyhow!("HLS master playlist has no variants"))?;
            let abs = base
                .join(&variant.uri)
                .with_context(|| format!("resolve variant uri {}", variant.uri))?;
            Err(anyhow!("HLS master playlist; re-fetch variant: {abs}"))
        }
        Ok(m3u8_rs::Playlist::MediaPlaylist(media)) => parse_hls_media(media, base),
        Err(e) => Err(anyhow!("m3u8 parse: {e}")),
    }
}

fn parse_hls_media(media: m3u8_rs::MediaPlaylist, base: Url) -> Result<ManifestSnapshot> {
    let is_live = !media.end_list;
    let mut init_url: Option<String> = None;
    if let Some(map) = media.segments.iter().find_map(|s| s.map.as_ref()) {
        let abs = base
            .join(&map.uri)
            .with_context(|| format!("resolve init uri {}", map.uri))?;
        init_url = Some(abs.to_string());
    }
    let mut segs = Vec::with_capacity(media.segments.len());
    for (i, s) in media.segments.iter().enumerate() {
        let abs = base
            .join(&s.uri)
            .with_context(|| format!("resolve seg uri {}", s.uri))?;
        let container = if init_url.is_some() {
            ContainerHint::Fmp4
        } else if s.uri.to_ascii_lowercase().ends_with(".ts") {
            ContainerHint::MpegTs
        } else {
            ContainerHint::Unknown
        };
        segs.push(SegmentRef {
            seq: media.media_sequence + i as u64,
            url: abs.to_string(),
            duration: s.duration as f64,
            container,
        });
    }

    let refresh_interval = if is_live {
        let secs = (media.target_duration as f64 / 2.0).max(1.5);
        Duration::from_secs_f64(secs)
    } else {
        Duration::from_secs(3600)
    };
    let duration = if is_live {
        None
    } else {
        Some(segs.iter().map(|s| s.duration).sum())
    };
    Ok(ManifestSnapshot {
        kind: ManifestKind::Hls,
        base_url: base,
        init_url,
        segments: segs,
        is_live,
        refresh_interval,
        duration,
    })
}

fn parse_dash(body: &str, base: Url) -> Result<ManifestSnapshot> {
    let mpd: dash_mpd::MPD = dash_mpd::parse(body).map_err(|e| anyhow!("DASH parse: {e}"))?;
    let is_live = matches!(mpd.mpdtype.as_deref(), Some("dynamic"));
    let period = mpd
        .periods
        .first()
        .ok_or_else(|| anyhow!("DASH MPD has no Period"))?;
    let adaptation = period
        .adaptations
        .iter()
        .find(|a| {
            a.contentType.as_deref() == Some("audio")
                || a.mimeType
                    .as_deref()
                    .map_or(false, |m| m.starts_with("audio/"))
        })
        .or_else(|| period.adaptations.first())
        .ok_or_else(|| anyhow!("DASH MPD has no AdaptationSet"))?;
    let representation = adaptation
        .representations
        .first()
        .ok_or_else(|| anyhow!("DASH Representation missing"))?;

    let st = adaptation
        .SegmentTemplate
        .as_ref()
        .or(representation.SegmentTemplate.as_ref())
        .ok_or_else(|| anyhow!("DASH SegmentTemplate missing — SegmentList not supported"))?;

    let timescale = st.timescale.unwrap_or(1) as f64;
    let seg_duration = st.duration.map(|d| d as f64 / timescale).unwrap_or(2.0);
    let start_number = st.startNumber.unwrap_or(1);

    let init_url = st.initialization.as_ref().map(|t| {
        let s = expand_template(t, representation.id.as_deref(), 0, representation.bandwidth);
        base.join(&s).map(|u| u.to_string()).unwrap_or(s)
    });

    let window = 6_u64;
    let mut segs = Vec::with_capacity(window as usize);
    let highest = if is_live {
        compute_live_head(&mpd, start_number, seg_duration).unwrap_or(start_number)
    } else {
        let total = period
            .duration
            .map(|d| d.as_secs_f64())
            .or_else(|| mpd.mediaPresentationDuration.map(|d| d.as_secs_f64()))
            .unwrap_or(0.0);
        let n = (total / seg_duration).ceil() as u64;
        start_number + n.saturating_sub(1)
    };
    let media_tmpl = st
        .media
        .as_ref()
        .ok_or_else(|| anyhow!("DASH SegmentTemplate.media missing"))?;
    let first = if is_live {
        highest.saturating_sub(window).max(start_number)
    } else {
        start_number
    };
    for n in first..=highest {
        let url = expand_template(
            media_tmpl,
            representation.id.as_deref(),
            n,
            representation.bandwidth,
        );
        let abs = base.join(&url).map(|u| u.to_string()).unwrap_or(url);
        segs.push(SegmentRef {
            seq: n,
            url: abs,
            duration: seg_duration,
            container: ContainerHint::Fmp4,
        });
    }

    let refresh_interval = if is_live {
        mpd.minimumUpdatePeriod
            .map(|d| Duration::from_secs_f64(d.as_secs_f64().max(1.5)))
            .unwrap_or_else(|| Duration::from_secs_f64(seg_duration))
    } else {
        Duration::from_secs(3600)
    };
    let duration = if is_live {
        None
    } else {
        mpd.mediaPresentationDuration.map(|d| d.as_secs_f64())
    };

    Ok(ManifestSnapshot {
        kind: ManifestKind::Dash,
        base_url: base,
        init_url,
        segments: segs,
        is_live,
        refresh_interval,
        duration,
    })
}

fn expand_template(tmpl: &str, rep_id: Option<&str>, number: u64, bandwidth: Option<u64>) -> String {
    let mut s = tmpl.to_string();
    if let Some(id) = rep_id {
        s = s.replace("$RepresentationID$", id);
    }
    s = s.replace("$Number$", &number.to_string());
    if let Some(bw) = bandwidth {
        s = s.replace("$Bandwidth$", &bw.to_string());
    }
    s
}

fn compute_live_head(mpd: &dash_mpd::MPD, start_number: u64, seg_duration: f64) -> Option<u64> {
    let ast = mpd.availabilityStartTime?;
    let now = unix_now()?;
    let elapsed = (now - ast.timestamp_millis() as f64 / 1000.0).max(0.0);
    let n = (elapsed / seg_duration).floor() as u64;
    Some(start_number + n)
}

fn unix_now() -> Option<f64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs_f64())
}
