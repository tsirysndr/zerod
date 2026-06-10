use std::sync::Arc;
use tonic::{Request, Response, Status};
use zerod_proto::v1alpha1::{
    systemd_service_server::SystemdService, ListManagedUnitsRequest, ListManagedUnitsResponse,
    StatusRequestUnit, StatusResponseUnit, UnitActiveState, UnitRequest, UnitResponse, UnitStatus,
};
use zerod_systemd::{ActiveState, UnitAllowlist};

pub struct SystemdSvc {
    allow: Arc<UnitAllowlist>,
}

impl SystemdSvc {
    pub fn new(allow: Arc<UnitAllowlist>) -> Self {
        Self { allow }
    }
}

fn err(e: anyhow::Error) -> Status {
    if e.to_string().contains("not in allowlist") {
        Status::permission_denied(e.to_string())
    } else {
        Status::internal(format!("{e:#}"))
    }
}

fn map_active(s: &str) -> UnitActiveState {
    match ActiveState::parse(s) {
        ActiveState::Active => UnitActiveState::Active,
        ActiveState::Reloading => UnitActiveState::Reloading,
        ActiveState::Inactive => UnitActiveState::Inactive,
        ActiveState::Failed => UnitActiveState::Failed,
        ActiveState::Activating => UnitActiveState::Activating,
        ActiveState::Deactivating => UnitActiveState::Deactivating,
        ActiveState::Unknown => UnitActiveState::Unspecified,
    }
}

fn to_proto(u: zerod_systemd::UnitStatus) -> UnitStatus {
    let active_state = map_active(&u.active_state) as i32;
    UnitStatus {
        name: u.name,
        description: u.description,
        load_state: u.load_state,
        active_state,
        sub_state: u.sub_state,
        enabled: u.enabled,
    }
}

#[tonic::async_trait]
impl SystemdService for SystemdSvc {
    async fn list_managed_units(
        &self,
        _req: Request<ListManagedUnitsRequest>,
    ) -> Result<Response<ListManagedUnitsResponse>, Status> {
        let units = zerod_systemd::list(&self.allow).await.map_err(err)?;
        Ok(Response::new(ListManagedUnitsResponse {
            units: units.into_iter().map(to_proto).collect(),
        }))
    }

    async fn status(
        &self,
        req: Request<StatusRequestUnit>,
    ) -> Result<Response<StatusResponseUnit>, Status> {
        let u = zerod_systemd::status(&self.allow, &req.into_inner().name)
            .await
            .map_err(err)?;
        Ok(Response::new(StatusResponseUnit {
            unit: Some(to_proto(u)),
        }))
    }

    async fn start(&self, req: Request<UnitRequest>) -> Result<Response<UnitResponse>, Status> {
        let name = req.into_inner().name;
        tracing::info!("systemd.Start {}", name);
        zerod_systemd::start(&self.allow, &name).await.map_err(err)?;
        Ok(Response::new(UnitResponse {}))
    }
    async fn stop(&self, req: Request<UnitRequest>) -> Result<Response<UnitResponse>, Status> {
        let name = req.into_inner().name;
        tracing::info!("systemd.Stop {}", name);
        zerod_systemd::stop(&self.allow, &name).await.map_err(err)?;
        Ok(Response::new(UnitResponse {}))
    }
    async fn restart(&self, req: Request<UnitRequest>) -> Result<Response<UnitResponse>, Status> {
        let name = req.into_inner().name;
        tracing::info!("systemd.Restart {}", name);
        zerod_systemd::restart(&self.allow, &name).await.map_err(err)?;
        Ok(Response::new(UnitResponse {}))
    }
    async fn reload(&self, req: Request<UnitRequest>) -> Result<Response<UnitResponse>, Status> {
        let name = req.into_inner().name;
        tracing::info!("systemd.Reload {}", name);
        zerod_systemd::reload(&self.allow, &name).await.map_err(err)?;
        Ok(Response::new(UnitResponse {}))
    }
    async fn enable(&self, req: Request<UnitRequest>) -> Result<Response<UnitResponse>, Status> {
        let name = req.into_inner().name;
        tracing::info!("systemd.Enable {}", name);
        zerod_systemd::enable(&self.allow, &name).await.map_err(err)?;
        Ok(Response::new(UnitResponse {}))
    }
    async fn disable(&self, req: Request<UnitRequest>) -> Result<Response<UnitResponse>, Status> {
        let name = req.into_inner().name;
        tracing::info!("systemd.Disable {}", name);
        zerod_systemd::disable(&self.allow, &name).await.map_err(err)?;
        Ok(Response::new(UnitResponse {}))
    }
}
