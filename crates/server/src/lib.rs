//! Tonic server wiring. `serve(settings)` boots all services on a single port.

mod bluetooth;
mod config;
mod settings;
mod stream;
mod system;
mod systemd;
mod volume;

pub use settings::{load_settings, Settings};

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tonic::transport::Server;
use zerod_proto::v1alpha1::{
    bluetooth_service_server::BluetoothServiceServer,
    config_service_server::ConfigServiceServer, stream_service_server::StreamServiceServer,
    system_service_server::SystemServiceServer, systemd_service_server::SystemdServiceServer,
    volume_service_server::VolumeServiceServer,
};

pub async fn serve(settings: Settings) -> Result<()> {
    let addr: SocketAddr = settings
        .server
        .bind
        .parse()
        .with_context(|| format!("parse bind {}", settings.server.bind))?;

    let allow = Arc::new(settings.systemd_allowlist());
    let registry = Arc::new(settings.config_registry());
    let bearer = resolve_bearer_token(&settings.server.bearer_token)?;

    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(zerod_proto::FILE_DESCRIPTOR_SET)
        .build_v1()
        .context("build reflection service")?;

    let interceptor = bearer_interceptor(bearer);

    tracing::info!(
        "zerod gRPC listening on {} ({} systemd unit(s), {} managed config(s))",
        addr,
        allow.units.len(),
        registry.list().len(),
    );
    for u in &allow.units {
        tracing::info!("  systemd allowlist: {}", u);
    }
    for c in registry.list() {
        tracing::info!("  config: {} → {} (unit={})", c.key, c.path.display(), c.unit);
    }
    Server::builder()
        .add_service(reflection)
        .add_service(SystemServiceServer::with_interceptor(
            system::SystemSvc::default(),
            interceptor.clone(),
        ))
        .add_service(BluetoothServiceServer::with_interceptor(
            bluetooth::BluetoothSvc::default(),
            interceptor.clone(),
        ))
        .add_service(StreamServiceServer::with_interceptor(
            stream::StreamSvc::default(),
            interceptor.clone(),
        ))
        .add_service(SystemdServiceServer::with_interceptor(
            systemd::SystemdSvc::new(allow.clone()),
            interceptor.clone(),
        ))
        .add_service(ConfigServiceServer::with_interceptor(
            config::ConfigSvc::new(registry, allow),
            interceptor.clone(),
        ))
        .add_service(VolumeServiceServer::with_interceptor(
            volume::VolumeSvc::default(),
            interceptor,
        ))
        .serve(addr)
        .await
        .context("tonic serve")?;
    Ok(())
}

/// Bearer-token check applied to every RPC. When `token` is empty, the
/// interceptor is a no-op.
fn bearer_interceptor(
    token: String,
) -> impl tonic::service::Interceptor + Clone {
    move |req: tonic::Request<()>| -> Result<tonic::Request<()>, tonic::Status> {
        if token.is_empty() {
            return Ok(req);
        }
        let expected = format!("Bearer {token}");
        match req.metadata().get("authorization") {
            Some(v) if v.to_str().map(|s| s == expected).unwrap_or(false) => Ok(req),
            _ => {
                tracing::warn!("rejected request: invalid or missing bearer token");
                Err(tonic::Status::unauthenticated("invalid bearer token"))
            }
        }
    }
}

/// Resolve the bearer token used by the server. Precedence:
///   1. `settings.server.bearer_token` (from zerod.toml)
///   2. `ZEROD_BEARER_TOKEN` env var
///   3. Generate a random 32-byte token and log it once at startup.
fn resolve_bearer_token(from_settings: &str) -> Result<String> {
    if !from_settings.is_empty() {
        tracing::info!("auth: using bearer token from zerod.toml");
        return Ok(from_settings.to_string());
    }
    if let Ok(t) = std::env::var("ZEROD_BEARER_TOKEN") {
        if !t.is_empty() {
            tracing::info!("auth: using bearer token from ZEROD_BEARER_TOKEN");
            return Ok(t);
        }
    }
    let token = random_hex_token()?;
    tracing::warn!(
        "auth: no bearer token configured — generated one for this run. \
         Set ZEROD_BEARER_TOKEN or zerod.toml [server].bearer_token to suppress this."
    );
    tracing::warn!("auth: BEARER TOKEN = {}", token);
    Ok(token)
}

fn random_hex_token() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).context("getrandom for bearer token")?;
    let mut s = String::with_capacity(buf.len() * 2);
    for b in buf {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").unwrap();
    }
    Ok(s)
}
