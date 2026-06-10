use tonic::{Request, Response, Status};
use zerod_proto::v1alpha1::{
    bluetooth_service_server::BluetoothService, BluetoothDevice, ConnectDeviceRequest,
    ConnectDeviceResponse, DisconnectRequest, DisconnectResponse, ListDevicesRequest,
    ListDevicesResponse, PairRequest, PairResponse, RemoveRequest, RemoveResponse, ScanRequest,
    ScanResponse,
};

#[derive(Default)]
pub struct BluetoothSvc;

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
}
