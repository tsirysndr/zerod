use tonic::{Request, Response, Status};
use zerod_proto::v1alpha1::{
    volume_service_server::VolumeService, GetVolumeRequest, GetVolumeResponse, ListMixersRequest,
    ListMixersResponse, MixerInfo, MixerSelector, SetMuteRequest, SetMuteResponse,
    SetVolumeRequest, SetVolumeResponse, VolumeStatus,
};
use zerod_volume::Selector;

#[derive(Default)]
pub struct VolumeSvc;

fn err(e: anyhow::Error) -> Status {
    Status::internal(format!("{e:#}"))
}

fn selector(m: Option<MixerSelector>) -> Selector {
    let m = m.unwrap_or_default();
    Selector::new(
        Some(m.card.as_str()),
        Some(m.control.as_str()),
        m.index,
    )
}

fn to_proto_info(m: zerod_volume::MixerInfo) -> MixerInfo {
    MixerInfo {
        card: m.card,
        control: m.control,
        index: m.index,
        has_volume: m.has_volume,
        has_switch: m.has_switch,
    }
}

fn to_proto_status(s: zerod_volume::VolumeStatus) -> VolumeStatus {
    VolumeStatus {
        volume_percent: s.volume_percent,
        muted: s.muted,
    }
}

#[tonic::async_trait]
impl VolumeService for VolumeSvc {
    async fn list_mixers(
        &self,
        req: Request<ListMixersRequest>,
    ) -> Result<Response<ListMixersResponse>, Status> {
        let card = req.into_inner().card;
        tracing::info!("volume.ListMixers card={:?}", card);
        let mixers = zerod_volume::list_mixers(card.as_deref()).map_err(err)?;
        tracing::info!("volume.ListMixers → {} mixer(s)", mixers.len());
        Ok(Response::new(ListMixersResponse {
            mixers: mixers.into_iter().map(to_proto_info).collect(),
        }))
    }

    async fn get_volume(
        &self,
        req: Request<GetVolumeRequest>,
    ) -> Result<Response<GetVolumeResponse>, Status> {
        let sel = selector(req.into_inner().mixer);
        tracing::info!("volume.GetVolume card={} control={}", sel.card, sel.control);
        let s = zerod_volume::get(&sel).map_err(err)?;
        Ok(Response::new(GetVolumeResponse {
            status: Some(to_proto_status(s)),
        }))
    }

    async fn set_volume(
        &self,
        req: Request<SetVolumeRequest>,
    ) -> Result<Response<SetVolumeResponse>, Status> {
        let req = req.into_inner();
        let sel = selector(req.mixer);
        let pct = req.volume_percent;
        tracing::info!(
            "volume.SetVolume card={} control={} pct={}",
            sel.card,
            sel.control,
            pct
        );
        zerod_volume::set_volume(&sel, pct).map_err(err)?;
        Ok(Response::new(SetVolumeResponse {}))
    }

    async fn set_mute(
        &self,
        req: Request<SetMuteRequest>,
    ) -> Result<Response<SetMuteResponse>, Status> {
        let req = req.into_inner();
        let sel = selector(req.mixer);
        tracing::info!(
            "volume.SetMute card={} control={} muted={}",
            sel.card,
            sel.control,
            req.muted
        );
        zerod_volume::set_mute(&sel, req.muted).map_err(err)?;
        Ok(Response::new(SetMuteResponse {}))
    }
}
