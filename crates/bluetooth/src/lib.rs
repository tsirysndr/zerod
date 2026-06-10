//! Linux bluetooth control via BlueZ (bluer). Non-Linux builds expose the
//! same API but every call returns `Err("bluetooth: linux only")`, so the
//! gRPC server can still compile on macOS/Windows for development.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BluetoothDevice {
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub rssi: Option<i32>,
}

#[cfg(target_os = "linux")]
mod imp {
    use super::BluetoothDevice;
    use anyhow::Result;
    use bluer::Address;
    use futures::{pin_mut, StreamExt};
    use std::str::FromStr;
    use std::time::Duration;
    use tracing::warn;

    async fn adapter() -> Result<bluer::Adapter> {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        adapter.set_powered(true).await?;
        Ok(adapter)
    }

    async fn device_info(adapter: &bluer::Adapter, addr: Address) -> Result<BluetoothDevice> {
        let device = adapter.device(addr)?;
        Ok(BluetoothDevice {
            address: addr.to_string(),
            name: device.name().await?.unwrap_or_default(),
            paired: device.is_paired().await?,
            trusted: device.is_trusted().await?,
            connected: device.is_connected().await?,
            rssi: device.rssi().await?.map(|v| v as i32),
        })
    }

    pub async fn scan(timeout_secs: u64) -> Result<Vec<BluetoothDevice>> {
        let adapter = adapter().await?;
        let secs = if timeout_secs == 0 { 10 } else { timeout_secs };
        {
            let discover = adapter.discover_devices().await?;
            pin_mut!(discover);
            let deadline = tokio::time::sleep(Duration::from_secs(secs));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    Some(_) = discover.next() => {}
                    else => break,
                }
            }
        }
        list_devices(&adapter).await
    }

    pub async fn get_devices() -> Result<Vec<BluetoothDevice>> {
        let adapter = adapter().await?;
        list_devices(&adapter).await
    }

    async fn list_devices(adapter: &bluer::Adapter) -> Result<Vec<BluetoothDevice>> {
        let addrs = adapter.device_addresses().await?;
        let mut devices = Vec::new();
        for addr in addrs {
            match device_info(adapter, addr).await {
                Ok(d) => devices.push(d),
                Err(e) => warn!("bluetooth: skipping {}: {}", addr, e),
            }
        }
        Ok(devices)
    }

    pub async fn pair(address: &str) -> Result<()> {
        let adapter = adapter().await?;
        let addr = Address::from_str(address)?;
        let device = adapter.device(addr)?;
        if !device.is_paired().await? {
            device.pair().await?;
        }
        if !device.is_trusted().await? {
            device.set_trusted(true).await?;
        }
        Ok(())
    }

    pub async fn connect(address: &str) -> Result<()> {
        let adapter = adapter().await?;
        let addr = Address::from_str(address)?;
        let device = adapter.device(addr)?;
        if !device.is_paired().await? {
            device.pair().await?;
        }
        if !device.is_trusted().await? {
            device.set_trusted(true).await?;
        }
        device.connect().await?;
        Ok(())
    }

    pub async fn disconnect(address: &str) -> Result<()> {
        let adapter = adapter().await?;
        let addr = Address::from_str(address)?;
        adapter.device(addr)?.disconnect().await?;
        Ok(())
    }

    pub async fn remove(address: &str) -> Result<()> {
        let adapter = adapter().await?;
        let addr = Address::from_str(address)?;
        adapter.remove_device(addr).await?;
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::BluetoothDevice;
    use anyhow::{bail, Result};

    pub async fn scan(_timeout_secs: u64) -> Result<Vec<BluetoothDevice>> {
        bail!("bluetooth: linux only")
    }
    pub async fn get_devices() -> Result<Vec<BluetoothDevice>> {
        bail!("bluetooth: linux only")
    }
    pub async fn pair(_address: &str) -> Result<()> {
        bail!("bluetooth: linux only")
    }
    pub async fn connect(_address: &str) -> Result<()> {
        bail!("bluetooth: linux only")
    }
    pub async fn disconnect(_address: &str) -> Result<()> {
        bail!("bluetooth: linux only")
    }
    pub async fn remove(_address: &str) -> Result<()> {
        bail!("bluetooth: linux only")
    }
}

pub use imp::{connect, disconnect, get_devices, pair, remove, scan};
