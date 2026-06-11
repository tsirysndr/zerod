//! `SnapcastService` — wraps the `zerod_snapcast::SnapcastClient` if
//! enabled in zerod.toml; otherwise every RPC returns
//! `FAILED_PRECONDITION` so reflection still lists the service.

use std::sync::Arc;
use tonic::{Request, Response, Status};
use zerod_proto::v1alpha1 as pb;
use zerod_proto::v1alpha1::snapcast_service_server::SnapcastService;
use zerod_snapcast::{SnapcastClient, SnapClient as SnapClientT, SnapGroup as SnapGroupT, SnapStream as SnapStreamT, SnapServer as SnapServerT};

pub struct SnapcastSvc {
    inner: Option<Arc<SnapcastClient>>,
}

impl SnapcastSvc {
    pub fn enabled(client: Arc<SnapcastClient>) -> Self {
        Self { inner: Some(client) }
    }

    pub fn disabled() -> Self {
        Self { inner: None }
    }

    fn client(&self) -> Result<&SnapcastClient, Status> {
        self.inner
            .as_deref()
            .ok_or_else(|| Status::failed_precondition("snapcast disabled in zerod.toml"))
    }
}

fn rpc_err(e: anyhow::Error) -> Status {
    Status::unavailable(format!("snapcast: {e:#}"))
}

#[tonic::async_trait]
impl SnapcastService for SnapcastSvc {
    async fn get_server_status(
        &self,
        _req: Request<pb::GetServerStatusRequest>,
    ) -> Result<Response<pb::GetServerStatusResponse>, Status> {
        let s = self.client()?.get_server_status().await.map_err(rpc_err)?;
        Ok(Response::new(pb::GetServerStatusResponse {
            server: Some(to_pb_server(&s)),
        }))
    }

    async fn list_clients(
        &self,
        _req: Request<pb::ListClientsRequest>,
    ) -> Result<Response<pb::ListClientsResponse>, Status> {
        let s = self.client()?.get_server_status().await.map_err(rpc_err)?;
        let mut clients = Vec::new();
        for g in &s.groups {
            for c in &g.clients {
                clients.push(to_pb_client(c));
            }
        }
        Ok(Response::new(pb::ListClientsResponse { clients }))
    }

    async fn list_snap_streams(
        &self,
        _req: Request<pb::ListSnapStreamsRequest>,
    ) -> Result<Response<pb::ListSnapStreamsResponse>, Status> {
        let s = self.client()?.get_server_status().await.map_err(rpc_err)?;
        let streams = s.streams.iter().map(to_pb_stream).collect();
        Ok(Response::new(pb::ListSnapStreamsResponse { streams }))
    }

    async fn set_client_volume(
        &self,
        req: Request<pb::SetClientVolumeRequest>,
    ) -> Result<Response<pb::SetClientVolumeResponse>, Status> {
        let r = req.into_inner();
        self.client()?
            .set_client_volume(&r.client_id, r.volume_percent, r.muted)
            .await
            .map_err(rpc_err)?;
        Ok(Response::new(pb::SetClientVolumeResponse {}))
    }

    async fn set_client_latency(
        &self,
        req: Request<pb::SetClientLatencyRequest>,
    ) -> Result<Response<pb::SetClientLatencyResponse>, Status> {
        let r = req.into_inner();
        self.client()?
            .set_client_latency(&r.client_id, r.latency_ms)
            .await
            .map_err(rpc_err)?;
        Ok(Response::new(pb::SetClientLatencyResponse {}))
    }

    async fn set_client_name(
        &self,
        req: Request<pb::SetClientNameRequest>,
    ) -> Result<Response<pb::SetClientNameResponse>, Status> {
        let r = req.into_inner();
        self.client()?
            .set_client_name(&r.client_id, &r.name)
            .await
            .map_err(rpc_err)?;
        Ok(Response::new(pb::SetClientNameResponse {}))
    }

    async fn set_group_stream(
        &self,
        req: Request<pb::SetGroupStreamRequest>,
    ) -> Result<Response<pb::SetGroupStreamResponse>, Status> {
        let r = req.into_inner();
        self.client()?
            .set_group_stream(&r.group_id, &r.stream_id)
            .await
            .map_err(rpc_err)?;
        Ok(Response::new(pb::SetGroupStreamResponse {}))
    }

    async fn set_group_mute(
        &self,
        req: Request<pb::SetGroupMuteRequest>,
    ) -> Result<Response<pb::SetGroupMuteResponse>, Status> {
        let r = req.into_inner();
        self.client()?
            .set_group_mute(&r.group_id, r.muted)
            .await
            .map_err(rpc_err)?;
        Ok(Response::new(pb::SetGroupMuteResponse {}))
    }

    async fn set_group_clients(
        &self,
        req: Request<pb::SetGroupClientsRequest>,
    ) -> Result<Response<pb::SetGroupClientsResponse>, Status> {
        let r = req.into_inner();
        self.client()?
            .set_group_clients(&r.group_id, r.client_ids)
            .await
            .map_err(rpc_err)?;
        Ok(Response::new(pb::SetGroupClientsResponse {}))
    }
}

fn to_pb_server(s: &SnapServerT) -> pb::SnapServer {
    pb::SnapServer {
        groups: s.groups.iter().map(to_pb_group).collect(),
        streams: s.streams.iter().map(to_pb_stream).collect(),
    }
}

fn to_pb_group(g: &SnapGroupT) -> pb::SnapGroup {
    pb::SnapGroup {
        id: g.id.clone(),
        name: g.name.clone(),
        stream_id: g.stream_id.clone(),
        muted: g.muted,
        client_ids: g.clients.iter().map(|c| c.id.clone()).collect(),
    }
}

fn to_pb_client(c: &SnapClientT) -> pb::SnapClient {
    pb::SnapClient {
        id: c.id.clone(),
        name: c.display_name().to_string(),
        connected: c.connected,
        volume_percent: c.config.volume.percent,
        muted: c.config.volume.muted,
        latency_ms: c.config.latency,
        mac: c.host.mac.clone(),
        host: c.host.name.clone(),
        version: c.snapclient.version.clone(),
    }
}

fn to_pb_stream(s: &SnapStreamT) -> pb::SnapStream {
    pb::SnapStream {
        id: s.id.clone(),
        status: s.status.clone(),
    }
}
