//! Long-lived JSON-RPC 2.0 client.
//!
//! Architecture: one supervisor task drives a single TCP connection.
//! Caller-side `call_*` methods talk to the supervisor via an unbounded
//! mpsc; the supervisor select!s between socket-reads and command-sends.
//! Pending request ids live in a `HashMap<u64, oneshot::Sender>` owned by
//! the supervisor — on disconnect the map is dropped, every in-flight
//! caller observes `oneshot::Receiver::Canceled`, and the supervisor
//! reconnects with backoff.

use anyhow::{anyhow, bail, Context as _, Result};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use crate::types::{ServerStatusResult, SnapServer};

pub struct SnapcastClient {
    cmd_tx: mpsc::UnboundedSender<Command>,
}

struct Command {
    method: &'static str,
    params: Value,
    reply: oneshot::Sender<Result<Value>>,
}

impl SnapcastClient {
    /// Spawn the supervisor task. Drop the returned `SnapcastClient` to
    /// stop it (the unbounded sender closes; the supervisor returns Ok).
    pub fn spawn(host: String, port: u16, forward_notifications: bool) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(supervisor(host, port, forward_notifications, cmd_rx));
        Self { cmd_tx }
    }

    pub async fn call_raw(&self, method: &'static str, params: Value) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command {
                method,
                params,
                reply: tx,
            })
            .map_err(|_| anyhow!("snapcast: supervisor task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("snapcast: connection dropped before reply"))?
    }

    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<T> {
        let v = self.call_raw(method, params).await?;
        serde_json::from_value(v)
            .map_err(|e| anyhow!("snapcast: parse {method} result: {e}"))
    }

    pub async fn get_server_status(&self) -> Result<SnapServer> {
        let r: ServerStatusResult = self.call("Server.GetStatus", Value::Null).await?;
        Ok(r.server)
    }

    pub async fn set_client_volume(&self, id: &str, percent: u32, muted: bool) -> Result<()> {
        self.call_raw(
            "Client.SetVolume",
            json!({ "id": id, "volume": { "percent": percent.min(100), "muted": muted } }),
        )
        .await?;
        Ok(())
    }

    pub async fn set_client_latency(&self, id: &str, latency_ms: i32) -> Result<()> {
        self.call_raw(
            "Client.SetLatency",
            json!({ "id": id, "latency": latency_ms }),
        )
        .await?;
        Ok(())
    }

    pub async fn set_client_name(&self, id: &str, name: &str) -> Result<()> {
        self.call_raw("Client.SetName", json!({ "id": id, "name": name }))
            .await?;
        Ok(())
    }

    pub async fn set_group_stream(&self, group_id: &str, stream_id: &str) -> Result<()> {
        self.call_raw(
            "Group.SetStream",
            json!({ "id": group_id, "stream_id": stream_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn set_group_mute(&self, group_id: &str, muted: bool) -> Result<()> {
        self.call_raw("Group.SetMute", json!({ "id": group_id, "mute": muted }))
            .await?;
        Ok(())
    }

    pub async fn set_group_clients(&self, group_id: &str, client_ids: Vec<String>) -> Result<()> {
        self.call_raw(
            "Group.SetClients",
            json!({ "id": group_id, "clients": client_ids }),
        )
        .await?;
        Ok(())
    }
}

async fn supervisor(
    host: String,
    port: u16,
    forward: bool,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
) {
    let mut backoff = Duration::from_millis(500);
    loop {
        match TcpStream::connect((host.as_str(), port)).await {
            Ok(sock) => {
                tracing::info!("snapcast: connected to {host}:{port}");
                backoff = Duration::from_millis(500);
                match run_connection(sock, forward, &mut cmd_rx).await {
                    Ok(()) => return,
                    Err(e) => tracing::warn!("snapcast: connection ended: {e:#}"),
                }
            }
            Err(e) => {
                tracing::warn!(
                    "snapcast: connect {host}:{port} failed: {e}; retrying in {backoff:?}"
                );
            }
        }
        // While we sleep before the next connect, refuse any commands
        // instead of letting them queue silently. Without this, RPCs on
        // a never-up snapserver hang indefinitely.
        let deadline = tokio::time::sleep(backoff);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { return; };
                    let _ = cmd.reply.send(Err(anyhow!("snapcast: not connected")));
                }
            }
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn run_connection(
    sock: TcpStream,
    forward: bool,
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
) -> Result<()> {
    let (rd, mut wr) = sock.into_split();
    let mut lines = BufReader::new(rd).lines();
    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut next_id: u64 = 1;

    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(s)) => {
                    if let Err(e) = handle_inbound(&s, &pending, forward) {
                        tracing::debug!("snapcast: bad inbound: {e}");
                    }
                }
                Ok(None) => return Err(anyhow!("connection closed")),
                Err(e) => return Err(anyhow!("read error: {e}")),
            },
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    // Owner dropped the client; clean shutdown.
                    return Ok(());
                };
                let id = next_id;
                next_id = next_id.wrapping_add(1);
                let body = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": cmd.method,
                    "params": cmd.params,
                });
                let mut bytes = serde_json::to_vec(&body)?;
                bytes.push(b'\r');
                bytes.push(b'\n');
                pending.lock().unwrap().insert(id, cmd.reply);
                if let Err(e) = wr.write_all(&bytes).await {
                    return Err(anyhow!("write error: {e}"));
                }
            }
        }
    }
}

fn handle_inbound(
    line: &str,
    pending: &Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    forward: bool,
) -> Result<()> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }
    let v: Value = serde_json::from_str(line).context("parse inbound json")?;
    if let Some(id_v) = v.get("id") {
        // Response to a prior request.
        let Some(id) = id_v.as_u64() else {
            bail!("response with non-numeric id");
        };
        let reply = pending.lock().unwrap().remove(&id);
        if let Some(tx) = reply {
            if let Some(err) = v.get("error") {
                let _ = tx.send(Err(anyhow!("snapcast rpc error: {err}")));
            } else if let Some(result) = v.get("result") {
                let _ = tx.send(Ok(result.clone()));
            } else {
                let _ = tx.send(Err(anyhow!("snapcast: response missing result and error")));
            }
        }
        Ok(())
    } else {
        // Notification (no id).
        if forward {
            let method = v.get("method").and_then(Value::as_str).unwrap_or("");
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            forward_notification(method, &params);
        }
        Ok(())
    }
}

fn forward_notification(method: &str, params: &Value) {
    match method {
        "Client.OnVolumeChanged" => {
            let id = params.get("id").and_then(Value::as_str).unwrap_or("");
            let vol = params.get("volume");
            let percent = vol
                .and_then(|v| v.get("percent"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let muted = vol
                .and_then(|v| v.get("muted"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            zerod_events::publish(zerod_events::Event::SnapcastClientChanged {
                client_id: id.to_string(),
                name: String::new(),
                volume_percent: percent,
                muted,
            });
        }
        _ => {
            tracing::debug!("snapcast: notification {method}");
        }
    }
}
