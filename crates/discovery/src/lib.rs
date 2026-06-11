//! mDNS service registration + browsing for zerod.
//!
//! Service type: `_zerod._tcp.local.`
//!
//! Servers call [`advertise`] once at startup and hold the returned
//! [`Advertisement`] for the daemon's lifetime — dropping it unregisters
//! the service.
//!
//! Clients call [`discover`] to browse the LAN with a short timeout.

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

pub const SERVICE_TYPE: &str = "_zerod._tcp.local.";

/// Live mDNS registration. The service is unregistered when this is dropped.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    /// The instance name as advertised (without the service-type suffix).
    pub fn instance_name(&self) -> &str {
        self.fullname
            .strip_suffix(&format!(".{}", SERVICE_TYPE))
            .unwrap_or(&self.fullname)
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        if let Err(e) = self.daemon.unregister(&self.fullname) {
            tracing::warn!("mDNS unregister failed: {e}");
        }
        if let Err(e) = self.daemon.shutdown() {
            tracing::warn!("mDNS shutdown failed: {e}");
        }
    }
}

/// Register zerod on the LAN under `instance_name` on the given port.
///
/// `instance_name` is the human-visible name (e.g. the machine hostname).
/// `txt` is included verbatim as TXT records — useful for version, etc.
pub fn advertise(instance_name: &str, port: u16, txt: &[(String, String)]) -> Result<Advertisement> {
    let daemon = ServiceDaemon::new().context("create mDNS daemon")?;
    let hostname = format!("{}.local.", sanitize(instance_name));
    let props: HashMap<String, String> = txt.iter().cloned().collect();

    // Empty IP list → mdns-sd auto-detects all non-loopback interfaces.
    let ips: &[IpAddr] = &[];

    let info = ServiceInfo::new(
        SERVICE_TYPE,
        instance_name,
        &hostname,
        ips,
        port,
        props,
    )
    .context("build mDNS ServiceInfo")?
    .enable_addr_auto();

    let fullname = info.get_fullname().to_string();
    daemon.register(info).context("register mDNS service")?;
    tracing::info!("mDNS: advertising {fullname} on port {port}");
    Ok(Advertisement { daemon, fullname })
}

/// One server discovered on the LAN.
#[derive(Debug, Clone)]
pub struct Discovered {
    /// Instance name as set by the server (e.g. the machine hostname).
    pub name: String,
    /// All addresses the server resolved on. Prefer the first IPv4 entry.
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    /// TXT records the server published.
    pub properties: HashMap<String, String>,
}

impl Discovered {
    /// Pick the best IPv4 to connect to. Skips loopback and the Docker default
    /// bridge range (172.17.0.0/16) — those leak in when zerod runs inside
    /// Docker or when docker0 is up on the host. IPv6 is ignored entirely.
    pub fn best_host(&self) -> Option<String> {
        let v4s: Vec<std::net::Ipv4Addr> = self
            .addresses
            .iter()
            .filter_map(|ip| match ip {
                IpAddr::V4(v4) => Some(*v4),
                _ => None,
            })
            .filter(|v4| !v4.is_loopback())
            .collect();

        let pick = v4s.iter().find(|v4| !is_docker_default(v4)).or_else(|| v4s.first());
        pick.map(|v4| v4.to_string())
    }
}

/// Docker's default bridge is 172.17.0.0/16. Filtering the rest of 172.16/12
/// would catch legitimate corporate / home LANs, so we only blacklist the
/// well-known Docker default.
fn is_docker_default(v4: &std::net::Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 172 && o[1] == 17
}

/// Browse for `_zerod._tcp.local.` on the LAN for `timeout`.
///
/// Returns every distinct instance that resolved within the window.
pub fn discover(timeout: Duration) -> Result<Vec<Discovered>> {
    let daemon = ServiceDaemon::new().context("create mDNS daemon")?;
    let receiver = daemon.browse(SERVICE_TYPE).context("start mDNS browse")?;

    let deadline = std::time::Instant::now() + timeout;
    let mut found: HashMap<String, Discovered> = HashMap::new();
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        match receiver.recv_timeout(deadline - now) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let name = info
                    .get_fullname()
                    .strip_suffix(&format!(".{}", SERVICE_TYPE))
                    .unwrap_or(info.get_fullname())
                    .to_string();
                let addresses: Vec<IpAddr> =
                    info.get_addresses().iter().map(|s| s.to_ip_addr()).collect();
                let properties = info
                    .get_properties()
                    .iter()
                    .map(|p| (p.key().to_string(), p.val_str().to_string()))
                    .collect();
                found.insert(
                    name.clone(),
                    Discovered {
                        name,
                        addresses,
                        port: info.get_port(),
                        properties,
                    },
                );
            }
            Ok(_) => {}
            Err(_) => break, // timeout
        }
    }

    let _ = daemon.shutdown();
    let mut out: Vec<Discovered> = found.into_values().collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// mdns-sd disallows whitespace and a few control chars in hostnames; replace them with `-`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '-' })
        .collect()
}
