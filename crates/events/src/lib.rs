//! Lossy in-process event bus. Subsystems call [`publish`]; the server fans
//! out to gRPC subscribers via [`subscribe`].
//!
//! Living in its own leaf crate sidesteps the dependency cycle: `stream`,
//! `bluetooth`, `systemd`, and `volume` can't depend on `zerod-server`, and
//! `zerod-proto` is codegen-only.

use once_cell::sync::Lazy;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

/// Capacity of the broadcast ring. Slow subscribers see `Lagged(n)`; the
/// server maps that into a synthetic `LaggedEvent` rather than dropping
/// the stream.
const BUS_CAPACITY: usize = 512;

static BUS: Lazy<broadcast::Sender<Envelope>> = Lazy::new(|| broadcast::channel(BUS_CAPACITY).0);

/// Publish-time timestamp + payload. Stamping at emission gives every
/// subscriber the same time even though `broadcast` may drop messages
/// between sender and receiver.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub timestamp_ms: i64,
    pub event: Event,
}

#[derive(Debug, Clone)]
pub enum Event {
    StreamStateChanged {
        state: StreamState,
        url: String,
        error: Option<String>,
    },
    StreamVolumeChanged {
        volume_percent: u32,
    },
    BluetoothDeviceChanged {
        address: String,
        name: String,
        paired: bool,
        connected: bool,
        trusted: bool,
    },
    BluetoothPairingRequest {
        address: String,
        passkey: Option<u32>,
    },
    BluetoothA2dpConnected {
        address: String,
        name: String,
    },
    BluetoothA2dpDisconnected {
        address: String,
        name: String,
    },
    SystemdUnitState {
        name: String,
        active_state: String,
        sub_state: String,
        enabled: bool,
    },
    VolumeChanged {
        card: String,
        control: String,
        volume_percent: u32,
        muted: bool,
    },
    SnapcastClientChanged {
        client_id: String,
        name: String,
        volume_percent: u32,
        muted: bool,
    },
    LibrespotStateChanged {
        state: String,
        track: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    Stopped,
    Buffering,
    Playing,
    Paused,
    Errored,
}

impl Event {
    /// Stable kind label used for client-side filtering. Keep these in sync
    /// with the docs for `EventsService.Subscribe`.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::StreamStateChanged { .. } => "stream.state",
            Event::StreamVolumeChanged { .. } => "stream.volume",
            Event::BluetoothDeviceChanged { .. } => "bt.device",
            Event::BluetoothPairingRequest { .. } => "bt.pairing",
            Event::BluetoothA2dpConnected { .. } => "bt.a2dp.connected",
            Event::BluetoothA2dpDisconnected { .. } => "bt.a2dp.disconnected",
            Event::SystemdUnitState { .. } => "systemd.unit",
            Event::VolumeChanged { .. } => "volume",
            Event::SnapcastClientChanged { .. } => "snap.client",
            Event::LibrespotStateChanged { .. } => "librespot.state",
        }
    }
}

/// Non-blocking emit. Safe to call while holding any Mutex — `broadcast::Sender::send`
/// never blocks. Silently drops if there are no subscribers (which is the
/// common case at startup).
pub fn publish(event: Event) {
    let env = Envelope {
        timestamp_ms: now_ms(),
        event,
    };
    let _ = BUS.send(env);
}

pub fn subscribe() -> broadcast::Receiver<Envelope> {
    BUS.subscribe()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
