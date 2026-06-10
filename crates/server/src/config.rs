use std::sync::Arc;
use tonic::{Request, Response, Status};
use zerod_config::{PostWriteAction, Registry};
use zerod_proto::v1alpha1::{
    config_service_server::ConfigService, GetConfigRequest, GetConfigResponse, ListConfigsRequest,
    ListConfigsResponse, ManagedConfig as ProtoManaged, PostWriteAction as ProtoAction,
    PutConfigRequest, PutConfigResponse,
};
use zerod_systemd::UnitAllowlist;

pub struct ConfigSvc {
    registry: Arc<Registry>,
    allow: Arc<UnitAllowlist>,
}

impl ConfigSvc {
    pub fn new(registry: Arc<Registry>, allow: Arc<UnitAllowlist>) -> Self {
        Self { registry, allow }
    }
}

fn to_proto(m: zerod_config::ManagedConfig) -> ProtoManaged {
    ProtoManaged {
        key: m.key,
        path: m.path.display().to_string(),
        unit: m.unit,
    }
}

fn err(e: anyhow::Error) -> Status {
    if e.to_string().contains("not in allowlist") || e.to_string().contains("not in registry") {
        Status::permission_denied(e.to_string())
    } else {
        Status::internal(format!("{e:#}"))
    }
}

fn map_action(p: i32) -> PostWriteAction {
    match ProtoAction::try_from(p) {
        Ok(ProtoAction::None) | Ok(ProtoAction::Unspecified) => PostWriteAction::None,
        Ok(ProtoAction::Reload) => PostWriteAction::Reload,
        Ok(ProtoAction::Restart) => PostWriteAction::Restart,
        Err(_) => PostWriteAction::None,
    }
}

#[tonic::async_trait]
impl ConfigService for ConfigSvc {
    async fn list_configs(
        &self,
        _req: Request<ListConfigsRequest>,
    ) -> Result<Response<ListConfigsResponse>, Status> {
        let configs = self.registry.list();
        tracing::info!("config.ListConfigs → {} entry(ies)", configs.len());
        Ok(Response::new(ListConfigsResponse {
            configs: configs.into_iter().map(to_proto).collect(),
        }))
    }

    async fn get_config(
        &self,
        req: Request<GetConfigRequest>,
    ) -> Result<Response<GetConfigResponse>, Status> {
        let key = req.into_inner().key;
        tracing::info!("config.GetConfig {}", key);
        let (mc, content) = self.registry.read(&key).await.map_err(err)?;
        Ok(Response::new(GetConfigResponse {
            config: Some(to_proto(mc)),
            content,
        }))
    }

    async fn put_config(
        &self,
        req: Request<PutConfigRequest>,
    ) -> Result<Response<PutConfigResponse>, Status> {
        let req = req.into_inner();
        let action = map_action(req.action);
        tracing::info!(
            "config.PutConfig {} ({} bytes) action={:?}",
            req.key,
            req.content.len(),
            action
        );
        let mc = self.registry.write(&req.key, &req.content).await.map_err(err)?;
        tracing::info!("config.PutConfig {} wrote {}", req.key, mc.path.display());
        let action_applied = zerod_config::apply_action(&mc, action, &self.allow)
            .await
            .map_err(err)?;
        if action_applied {
            tracing::info!("config.PutConfig {} post-write action applied", req.key);
        }
        Ok(Response::new(PutConfigResponse { action_applied }))
    }
}
