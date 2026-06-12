//! Symphonia-based demux + decode. The decoder instance is persistent
//! across segments so AAC encoder priming, predictor state, and SBR/PS
//! continuity are preserved — same model ffplay uses against libavcodec.

use anyhow::{anyhow, Result};
use bytes::Bytes;
use std::io::Cursor;
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{CodecParameters, DecoderOptions, CODEC_TYPE_AAC};
use symphonia::core::conv::{ConvertibleSample, IntoSample};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::sample::Sample;

pub struct DecodedSegment {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Stateful decoder reused across every segment of a stream. The format
/// reader is built fresh per segment (so each segment's container can be
/// probed independently), but the AAC `Decoder` lives for the whole
/// playback — that's what eliminates the click at every segment boundary
/// you get when you `make()` a new decoder per segment.
pub struct StreamDecoder {
    decoder: Option<Box<dyn symphonia::core::codecs::Decoder>>,
    last_params: Option<CodecParameters>,
}

impl Default for StreamDecoder {
    fn default() -> Self {
        Self {
            decoder: None,
            last_params: None,
        }
    }
}

impl StreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn decode_segment(
        &mut self,
        init: Option<&[u8]>,
        segment: &[u8],
    ) -> Result<DecodedSegment> {
        let mut data = Vec::with_capacity(init.map_or(0, |i| i.len()) + segment.len());
        if let Some(i) = init {
            data.extend_from_slice(i);
        }
        data.extend_from_slice(segment);
        let cursor = Cursor::new(Bytes::from(data));
        let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

        let probed = symphonia::default::get_probe()
            .format(
                &Hint::new(),
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| anyhow!("probe: {e}"))?;
        let mut format = probed.format;

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec == CODEC_TYPE_AAC)
            .ok_or_else(|| anyhow!("no AAC track in segment"))?;
        let track_id = track.id;
        let codec_params = track.codec_params.clone();

        // Build the decoder on first segment, or rebuild if the codec
        // params changed mid-stream (a profile switch — rare but possible
        // in HLS with bitrate ladders). Anything else reuses the existing
        // decoder so its internal state carries over.
        let need_rebuild = match &self.last_params {
            None => true,
            Some(prev) => {
                prev.codec != codec_params.codec
                    || prev.sample_rate != codec_params.sample_rate
                    || prev.channels != codec_params.channels
            }
        };
        if need_rebuild {
            self.decoder = Some(
                symphonia::default::get_codecs()
                    .make(&codec_params, &DecoderOptions::default())
                    .map_err(|e| anyhow!("make decoder: {e}"))?,
            );
            self.last_params = Some(codec_params);
        }
        let decoder = self.decoder.as_mut().expect("decoder built above");

        let mut samples: Vec<i16> = Vec::with_capacity(8192);
        let mut sample_rate = self
            .last_params
            .as_ref()
            .and_then(|c| c.sample_rate)
            .unwrap_or(44_100);
        let mut channels = self
            .last_params
            .as_ref()
            .and_then(|c| c.channels)
            .map(|c| c.count() as u16)
            .unwrap_or(2);

        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break
                }
                Err(SymphoniaError::ResetRequired) => break,
                Err(e) => return Err(anyhow!("next_packet: {e}")),
            };
            if packet.track_id() != track_id {
                continue;
            }
            let decoded = match decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymphoniaError::DecodeError(msg)) => {
                    tracing::warn!("stream/decoder: drop bad frame: {msg}");
                    continue;
                }
                Err(e) => return Err(anyhow!("decode: {e}")),
            };
            let spec = *decoded.spec();
            sample_rate = spec.rate;
            channels = spec.channels.count() as u16;
            append_interleaved_s16(&decoded, &mut samples);
        }

        Ok(DecodedSegment {
            samples,
            sample_rate,
            channels,
        })
    }
}

fn append_interleaved_s16(decoded: &AudioBufferRef<'_>, out: &mut Vec<i16>) {
    match decoded {
        AudioBufferRef::U8(b) => copy(b.as_ref(), out),
        AudioBufferRef::U16(b) => copy(b.as_ref(), out),
        AudioBufferRef::U24(b) => copy(b.as_ref(), out),
        AudioBufferRef::U32(b) => copy(b.as_ref(), out),
        AudioBufferRef::S8(b) => copy(b.as_ref(), out),
        AudioBufferRef::S16(b) => copy(b.as_ref(), out),
        AudioBufferRef::S24(b) => copy(b.as_ref(), out),
        AudioBufferRef::S32(b) => copy(b.as_ref(), out),
        AudioBufferRef::F32(b) => copy(b.as_ref(), out),
        AudioBufferRef::F64(b) => copy(b.as_ref(), out),
    }
}

fn copy<S>(buf: &symphonia::core::audio::AudioBuffer<S>, out: &mut Vec<i16>)
where
    S: Sample + ConvertibleSample + IntoSample<i16> + Copy,
{
    let spec = buf.spec();
    let ch = spec.channels.count();
    let frames = buf.frames();
    out.reserve(frames * ch);
    for f in 0..frames {
        for c in 0..ch {
            let plane = buf.chan(c);
            out.push(plane[f].into_sample());
        }
    }
}
