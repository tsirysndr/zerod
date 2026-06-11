use std::sync::Arc;
use tonic::{Request, Response, Status};
use zerod_proto::v1alpha1::{
    bluetooth_service_server::BluetoothService, A2dpDisableRequest, A2dpDisableResponse,
    A2dpEnableRequest, A2dpEnableResponse, BluetoothDevice, ConnectDeviceRequest,
    ConnectDeviceResponse, DisconnectRequest, DisconnectResponse, ListDevicesRequest,
    ListDevicesResponse, PairRequest, PairResponse, RemoveRequest, RemoveResponse,
    RespondPairingRequest, RespondPairingResponse, ScanRequest, ScanResponse,
    SetDiscoverableRequest, SetDiscoverableResponse,
};
use zerod_systemd::UnitAllowlist;

use crate::settings::A2dpSettings;

pub struct BluetoothSvc {
    a2dp: A2dpSettings,
    allow: Arc<UnitAllowlist>,
}

impl BluetoothSvc {
    pub fn new(a2dp: A2dpSettings, allow: Arc<UnitAllowlist>) -> Self {
        Self { a2dp, allow }
    }

    fn require_a2dp(&self) -> Result<(), Status> {
        if !self.a2dp.enabled {
            return Err(Status::failed_precondition(
                "A2DP disabled in zerod.toml ([bluetooth.a2dp].enabled = false)",
            ));
        }
        Ok(())
    }
}

fn to_proto(d: zerod_bluetooth::BluetoothDevice) -> BluetoothDevice {
    BluetoothDevice {
        address: d.address,
        name: d.name,
        paired: d.paired,
        trusted: d.trusted,
        connected: d.connected,
        rssi: d.rssi,
    }
}

fn err(e: anyhow::Error) -> Status {
    Status::internal(format!("{e:#}"))
}

#[tonic::async_trait]
impl BluetoothService for BluetoothSvc {
    async fn scan(&self, req: Request<ScanRequest>) -> Result<Response<ScanResponse>, Status> {
        let timeout = req.into_inner().timeout_secs as u64;
        tracing::info!("bluetooth.Scan timeout_secs={}", timeout);
        let devices = zerod_bluetooth::scan(timeout).await.map_err(err)?;
        tracing::info!("bluetooth.Scan → {} device(s)", devices.len());
        Ok(Response::new(ScanResponse {
            devices: devices.into_iter().map(to_proto).collect(),
        }))
    }

    async fn list_devices(
        &self,
        _req: Request<ListDevicesRequest>,
    ) -> Result<Response<ListDevicesResponse>, Status> {
        tracing::info!("bluetooth.ListDevices");
        let devices = zerod_bluetooth::get_devices().await.map_err(err)?;
        tracing::info!("bluetooth.ListDevices → {} device(s)", devices.len());
        Ok(Response::new(ListDevicesResponse {
            devices: devices.into_iter().map(to_proto).collect(),
        }))
    }

    async fn pair(&self, req: Request<PairRequest>) -> Result<Response<PairResponse>, Status> {
        let addr = req.into_inner().address;
        tracing::info!("bluetooth.Pair {}", addr);
        zerod_bluetooth::pair(&addr).await.map_err(err)?;
        tracing::info!("bluetooth.Pair {} ok", addr);
        Ok(Response::new(PairResponse {}))
    }

    async fn connect_device(
        &self,
        req: Request<ConnectDeviceRequest>,
    ) -> Result<Response<ConnectDeviceResponse>, Status> {
        let addr = req.into_inner().address;
        tracing::info!("bluetooth.Connect {}", addr);
        zerod_bluetooth::connect(&addr).await.map_err(err)?;
        tracing::info!("bluetooth.Connect {} ok", addr);
        Ok(Response::new(ConnectDeviceResponse {}))
    }

    async fn disconnect(
        &self,
        req: Request<DisconnectRequest>,
    ) -> Result<Response<DisconnectResponse>, Status> {
        let addr = req.into_inner().address;
        tracing::info!("bluetooth.Disconnect {}", addr);
        zerod_bluetooth::disconnect(&addr).await.map_err(err)?;
        Ok(Response::new(DisconnectResponse {}))
    }

    async fn remove(
        &self,
        req: Request<RemoveRequest>,
    ) -> Result<Response<RemoveResponse>, Status> {
        let addr = req.into_inner().address;
        tracing::info!("bluetooth.Remove {}", addr);
        zerod_bluetooth::remove(&addr).await.map_err(err)?;
        Ok(Response::new(RemoveResponse {}))
    }

    async fn set_discoverable(
        &self,
        req: Request<SetDiscoverableRequest>,
    ) -> Result<Response<SetDiscoverableResponse>, Status> {
        let r = req.into_inner();
        tracing::info!(
            "bluetooth.SetDiscoverable on={} timeout_secs={}",
            r.discoverable,
            r.timeout_secs,
        );
        zerod_bluetooth::set_discoverable(r.discoverable, r.timeout_secs)
            .await
            .map_err(err)?;
        Ok(Response::new(SetDiscoverableResponse {}))
    }

    async fn respond_pairing(
        &self,
        req: Request<RespondPairingRequest>,
    ) -> Result<Response<RespondPairingResponse>, Status> {
        let r = req.into_inner();
        tracing::info!("bluetooth.RespondPairing address={} accept={}", r.address, r.accept);
        zerod_bluetooth::respond_pairing(&r.address, r.accept)
            .await
            .map_err(|e| Status::failed_precondition(format!("{e:#}")))?;
        Ok(Response::new(RespondPairingResponse {}))
    }

    async fn a2dp_enable(
        &self,
        _req: Request<A2dpEnableRequest>,
    ) -> Result<Response<A2dpEnableResponse>, Status> {
        self.require_a2dp()?;
        let unit = &self.a2dp.bluealsa_aplay_unit;
        tracing::info!("bluetooth.A2dpEnable starting {}", unit);
        // Pre-flight: surface a clear error if bluez-alsa isn't installed
        // rather than a cryptic systemd "unit not found".
        if let Err(e) = zerod_systemd::status(&self.allow, unit).await {
            return Err(Status::failed_precondition(format!(
                "{unit} not available — install bluez-alsa-utils on the device ({e:#})"
            )));
        }
        zerod_systemd::start(&self.allow, unit).await.map_err(err)?;
        zerod_bluetooth::set_discoverable(true, self.a2dp.discoverable_timeout_secs)
            .await
            .map_err(err)?;
        Ok(Response::new(A2dpEnableResponse {}))
    }

    async fn a2dp_disable(
        &self,
        _req: Request<A2dpDisableRequest>,
    ) -> Result<Response<A2dpDisableResponse>, Status> {
        self.require_a2dp()?;
        let unit = &self.a2dp.bluealsa_aplay_unit;
        tracing::info!("bluetooth.A2dpDisable stopping {}", unit);
        // set_discoverable failure is non-fatal — we still try to stop
        // the unit so a partial state doesn't leave the daemon broadcasting.
        if let Err(e) = zerod_bluetooth::set_discoverable(false, 0).await {
            tracing::warn!("bluetooth.A2dpDisable set_discoverable(false) failed: {e:#}");
        }
        zerod_systemd::stop(&self.allow, unit).await.map_err(err)?;
        Ok(Response::new(A2dpDisableResponse {}))
    }
}
