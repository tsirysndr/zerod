//! BlueZ pairing agent + adapter setup for A2DP-sink mode.
//!
//! On `start`, this:
//!   * sets the adapter alias (so the phone sees a friendly name),
//!   * makes the adapter pairable and (optionally) discoverable,
//!   * registers a BlueZ pairing agent with bluer.
//!
//! The agent's `RequestConfirmation` callback publishes a
//! `BluetoothPairingRequest` event and either auto-accepts
//! (kiosk mode) or parks on a per-address oneshot waiting for a
//! `respond_pairing(address, accept)` call. A 30s timeout falls back
//! to rejection so a stuck prompt doesn't leak the channel.
//!
//! `AuthorizeService` auto-allows the A2DP Source UUID
//! (`0000110a-…`) so phones can stream audio without a second confirm
//! after pairing.

use anyhow::{anyhow, Context, Result};
use bluer::agent::{Agent, AgentHandle, ReqError, RequestConfirmation};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::oneshot;

use crate::A2dpConfig;

/// A2DP Source profile UUID (BlueZ "AudioSource"). Phones expose this
/// when they want to send audio to us.
const A2DP_SOURCE_UUID: &str = "0000110a-0000-1000-8000-00805f9b34fb";

static AGENT_HANDLE: Lazy<Mutex<Option<AgentHandle>>> = Lazy::new(|| Mutex::new(None));
static SESSION: Lazy<Mutex<Option<bluer::Session>>> = Lazy::new(|| Mutex::new(None));
static AUTO_ACCEPT: AtomicBool = AtomicBool::new(false);
static PENDING_PAIRINGS: Lazy<Mutex<HashMap<String, oneshot::Sender<bool>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub async fn start(cfg: A2dpConfig) -> Result<()> {
    if AGENT_HANDLE.lock().unwrap().is_some() {
        tracing::debug!("bluetooth: agent already registered; ignoring duplicate start");
        return Ok(());
    }
    AUTO_ACCEPT.store(cfg.auto_accept_pairings, Ordering::SeqCst);

    let session = bluer::Session::new().await.context("bluer session")?;
    let adapter = session
        .default_adapter()
        .await
        .context("bluer default_adapter")?;
    adapter.set_powered(true).await.context("set_powered")?;
    if !cfg.adapter_alias.is_empty() {
        if let Err(e) = adapter.set_alias(cfg.adapter_alias.clone()).await {
            tracing::warn!("bluetooth: set_alias({}) failed: {e}", cfg.adapter_alias);
        }
    }
    if let Err(e) = adapter.set_pairable(true).await {
        tracing::warn!("bluetooth: set_pairable(true) failed: {e}");
    }
    if cfg.discoverable_on_boot {
        if let Err(e) = adapter
            .set_discoverable_timeout(cfg.discoverable_timeout_secs)
            .await
        {
            tracing::warn!("bluetooth: set_discoverable_timeout failed: {e}");
        }
        if let Err(e) = adapter.set_discoverable(true).await {
            tracing::warn!("bluetooth: set_discoverable(true) failed: {e}");
        } else {
            tracing::info!(
                "bluetooth: adapter discoverable (timeout={}s, alias=\"{}\")",
                cfg.discoverable_timeout_secs,
                cfg.adapter_alias,
            );
        }
    }

    let agent = Agent {
        request_default: true,
        request_confirmation: Some(Box::new(|req: RequestConfirmation| {
            Box::pin(async move { handle_request_confirmation(req).await })
        })),
        authorize_service: Some(Box::new(|req: bluer::agent::AuthorizeService| {
            Box::pin(async move {
                let uuid = req.service.to_string();
                if uuid.eq_ignore_ascii_case(A2DP_SOURCE_UUID) {
                    tracing::info!(
                        "bluetooth: authorize A2DP source from {} (uuid {})",
                        req.device,
                        uuid,
                    );
                    zerod_events::publish(zerod_events::Event::BluetoothA2dpConnected {
                        address: req.device.to_string(),
                        name: String::new(),
                    });
                    Ok(())
                } else if AUTO_ACCEPT.load(Ordering::SeqCst) {
                    tracing::debug!(
                        "bluetooth: auto-authorize service {} from {}",
                        uuid,
                        req.device,
                    );
                    Ok(())
                } else {
                    tracing::debug!(
                        "bluetooth: rejecting service {} from {} (auto_accept=false)",
                        uuid,
                        req.device,
                    );
                    Err(ReqError::Rejected)
                }
            })
        })),
        ..Default::default()
    };

    let handle = session
        .register_agent(agent)
        .await
        .context("bluer register_agent")?;
    *AGENT_HANDLE.lock().unwrap() = Some(handle);
    // Keep the session alive — dropping it unregisters all proxies.
    *SESSION.lock().unwrap() = Some(session);
    tracing::info!(
        "bluetooth: pairing agent registered (auto_accept={})",
        cfg.auto_accept_pairings,
    );
    Ok(())
}

pub async fn stop() -> Result<()> {
    let _ = AGENT_HANDLE.lock().unwrap().take(); // dropping unregisters
    let _ = SESSION.lock().unwrap().take();
    // Reject any still-parked pairing prompts so their callbacks unwind.
    let pending: Vec<_> = {
        let mut g = PENDING_PAIRINGS.lock().unwrap();
        g.drain().collect()
    };
    for (_addr, tx) in pending {
        let _ = tx.send(false);
    }
    Ok(())
}

pub async fn set_discoverable(on: bool, timeout_secs: u32) -> Result<()> {
    let session = bluer::Session::new().await.context("bluer session")?;
    let adapter = session
        .default_adapter()
        .await
        .context("bluer default_adapter")?;
    adapter
        .set_discoverable_timeout(timeout_secs)
        .await
        .context("set_discoverable_timeout")?;
    adapter
        .set_discoverable(on)
        .await
        .context("set_discoverable")?;
    Ok(())
}

pub fn respond_pairing(address: &str, accept: bool) -> Result<()> {
    let tx = PENDING_PAIRINGS
        .lock()
        .unwrap()
        .remove(address)
        .ok_or_else(|| anyhow!("no pending pairing prompt for {address}"))?;
    let _ = tx.send(accept);
    Ok(())
}

async fn handle_request_confirmation(req: RequestConfirmation) -> Result<(), ReqError> {
    let address = req.device.to_string();
    let passkey = req.passkey;
    zerod_events::publish(zerod_events::Event::BluetoothPairingRequest {
        address: address.clone(),
        passkey: Some(passkey),
    });

    if AUTO_ACCEPT.load(Ordering::SeqCst) {
        tracing::info!(
            "bluetooth: auto-accepting pairing from {} (passkey={})",
            address,
            passkey,
        );
        zerod_events::publish(zerod_events::Event::BluetoothDeviceChanged {
            address: address.clone(),
            name: String::new(),
            paired: true,
            connected: false,
            trusted: false,
        });
        return Ok(());
    }

    let (tx, rx) = oneshot::channel();
    {
        let mut g = PENDING_PAIRINGS.lock().unwrap();
        // If a prior prompt is still pending for the same address, reject
        // it so the new one wins — phones retry quickly.
        if let Some(old) = g.insert(address.clone(), tx) {
            let _ = old.send(false);
        }
    }

    tracing::info!(
        "bluetooth: waiting for RespondPairing from client (address={}, passkey={})",
        address,
        passkey,
    );

    match tokio::time::timeout(Duration::from_secs(30), rx).await {
        Ok(Ok(true)) => {
            tracing::info!("bluetooth: pairing accepted by client for {}", address);
            zerod_events::publish(zerod_events::Event::BluetoothDeviceChanged {
                address: address.clone(),
                name: String::new(),
                paired: true,
                connected: false,
                trusted: false,
            });
            Ok(())
        }
        Ok(Ok(false)) => {
            tracing::info!("bluetooth: pairing rejected by client for {}", address);
            Err(ReqError::Rejected)
        }
        Ok(Err(_)) | Err(_) => {
            // Timeout or oneshot dropped — make sure we don't leak the
            // entry if the timeout branch fired first.
            PENDING_PAIRINGS.lock().unwrap().remove(&address);
            tracing::warn!(
                "bluetooth: pairing for {} timed out without response — rejecting",
                address,
            );
            Err(ReqError::Rejected)
        }
    }
}
