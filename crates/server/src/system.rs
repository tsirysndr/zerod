use tonic::{Request, Response, Status};
use zerod_proto::v1alpha1::{
    system_service_server::SystemService, HealthRequest, HealthResponse, VersionRequest,
    VersionResponse,
};

#[derive(Default)]
pub struct SystemSvc;

#[tonic::async_trait]
impl SystemService for SystemSvc {
    async fn version(
        &self,
        _req: Request<VersionRequest>,
    ) -> Result<Response<VersionResponse>, Status> {
        Ok(Response::new(VersionResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }))
    }

    async fn health(
        &self,
        _req: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        Ok(Response::new(HealthResponse { ok: true }))
    }
}
