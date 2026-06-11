use tonic::{Request, Response, Status};
use zerod_proto::v1alpha1::{
    stream_service_server::StreamService, AudioOutput as ProtoOutput, GetStreamVolumeRequest,
    GetStreamVolumeResponse, PauseRequest, PauseResponse, PlayRequest, PlayResponse,
    PlaybackSource as ProtoSource, PlayerState as ProtoState, ResumeRequest, ResumeResponse,
    SetStreamVolumeRequest, SetStreamVolumeResponse, SpotifyStartRequest, SpotifyStartResponse,
    SpotifyStopRequest, SpotifyStopResponse, StatusRequest, StatusResponse, StopRequest,
    StopResponse,
};
use zerod_stream::{AudioOutput, LibrespotConfig, PlayConfig, PlaybackSource, PlayerState};

use crate::settings::LibrespotSettings;

pub struct StreamSvc {
    librespot: LibrespotSettings,
}

impl StreamSvc {
    pub fn new(librespot: LibrespotSettings) -> Self {
        Self { librespot }
    }
}

fn map_output(
    output: i32,
    pipe_path: Option<String>,
    cpal_device: Option<String>,
) -> Result<AudioOutput, Status> {
    match ProtoOutput::try_from(output) {
        Ok(ProtoOutput::Cpal) | Ok(ProtoOutput::Unspecified) => Ok(AudioOutput::Cpal {
            device: cpal_device.filter(|s| !s.is_empty()),
        }),
        Ok(ProtoOutput::Stdout) => Ok(AudioOutput::Stdout),
        Ok(ProtoOutput::Pipe) => {
            let path = pipe_path
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Status::invalid_argument("pipe_path required for AUDIO_OUTPUT_PIPE"))?;
            Ok(AudioOutput::Pipe { path })
        }
        Err(_) => Err(Status::invalid_argument("unknown AudioOutput")),
    }
}

fn map_output_back(o: &AudioOutput) -> ProtoOutput {
    match o {
        AudioOutput::Cpal { .. } => ProtoOutput::Cpal,
        AudioOutput::Stdout => ProtoOutput::Stdout,
        AudioOutput::Pipe { .. } => ProtoOutput::Pipe,
    }
}

fn map_state(s: PlayerState) -> ProtoState {
    match s {
        PlayerState::Stopped => ProtoState::Stopped,
        PlayerState::Buffering => ProtoState::Buffering,
        PlayerState::Playing => ProtoState::Playing,
        PlayerState::Paused => ProtoState::Paused,
        PlayerState::Errored => ProtoState::Errored,
    }
}

fn map_source(s: PlaybackSource) -> ProtoSource {
    match s {
        PlaybackSource::Unspecified => ProtoSource::Unspecified,
        PlaybackSource::Hls => ProtoSource::Hls,
        PlaybackSource::Dash => ProtoSource::Dash,
        PlaybackSource::Spotify => ProtoSource::Spotify,
    }
}

#[tonic::async_trait]
impl StreamService for StreamSvc {
    async fn play(&self, req: Request<PlayRequest>) -> Result<Response<PlayResponse>, Status> {
        let req = req.into_inner();
        let output = map_output(req.output, req.pipe_path, req.cpal_device)?;
        tracing::info!("stream.Play url={} output={:?}", req.url, output);
        zerod_stream::play(PlayConfig {
            url: req.url,
            output,
        })
        .map_err(|e| Status::internal(format!("{e:#}")))?;
        Ok(Response::new(PlayResponse {}))
    }

    async fn pause(&self, _req: Request<PauseRequest>) -> Result<Response<PauseResponse>, Status> {
        tracing::info!("stream.Pause");
        zerod_stream::pause();
        Ok(Response::new(PauseResponse {}))
    }

    async fn resume(
        &self,
        _req: Request<ResumeRequest>,
    ) -> Result<Response<ResumeResponse>, Status> {
        tracing::info!("stream.Resume");
        zerod_stream::resume();
        Ok(Response::new(ResumeResponse {}))
    }

    async fn stop(&self, _req: Request<StopRequest>) -> Result<Response<StopResponse>, Status> {
        tracing::info!("stream.Stop");
        zerod_stream::stop();
        Ok(Response::new(StopResponse {}))
    }

    async fn status(
        &self,
        _req: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        let s = zerod_stream::status();
        Ok(Response::new(StatusResponse {
            state: map_state(s.state) as i32,
            url: s.url,
            position_ms: s.position_ms,
            duration_ms: s.duration_ms,
            is_live: s.is_live,
            error: s.error,
            output: map_output_back(&s.output) as i32,
            volume_percent: s.volume_percent,
            source: map_source(s.source) as i32,
        }))
    }

    async fn set_stream_volume(
        &self,
        req: Request<SetStreamVolumeRequest>,
    ) -> Result<Response<SetStreamVolumeResponse>, Status> {
        let pct = req.into_inner().volume_percent;
        zerod_stream::set_volume(pct);
        Ok(Response::new(SetStreamVolumeResponse {}))
    }

    async fn get_stream_volume(
        &self,
        _req: Request<GetStreamVolumeRequest>,
    ) -> Result<Response<GetStreamVolumeResponse>, Status> {
        Ok(Response::new(GetStreamVolumeResponse {
            volume_percent: zerod_stream::volume(),
        }))
    }

    async fn spotify_start(
        &self,
        req: Request<SpotifyStartRequest>,
    ) -> Result<Response<SpotifyStartResponse>, Status> {
        if !self.librespot.enabled {
            return Err(Status::failed_precondition(
                "librespot disabled in zerod.toml ([librespot].enabled = false)",
            ));
        }
        let req = req.into_inner();
        let output = map_output(req.output, req.pipe_path, req.cpal_device)?;
        let cfg = LibrespotConfig {
            binary: self.librespot.binary.clone(),
            name: self.librespot.name.clone(),
            bitrate: self.librespot.bitrate,
            cache_path: self.librespot.cache_path.clone(),
            output,
        };
        tracing::info!("stream.SpotifyStart name={}", cfg.name);
        zerod_stream::spotify_start(cfg).map_err(|e| Status::internal(format!("{e:#}")))?;
        Ok(Response::new(SpotifyStartResponse {}))
    }

    async fn spotify_stop(
        &self,
        _req: Request<SpotifyStopRequest>,
    ) -> Result<Response<SpotifyStopResponse>, Status> {
        tracing::info!("stream.SpotifyStop");
        zerod_stream::spotify_stop();
        Ok(Response::new(SpotifyStopResponse {}))
    }
}
