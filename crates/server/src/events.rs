//! `EventsService.Subscribe` — server-streaming view of the in-process bus.

use futures::Stream;
use std::pin::Pin;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};
use zerod_events::{Envelope, Event, StreamState};
use zerod_proto::v1alpha1 as pb;
use zerod_proto::v1alpha1::events_service_server::EventsService;

#[derive(Default)]
pub struct EventsSvc;

#[tonic::async_trait]
impl EventsService for EventsSvc {
    type SubscribeStream =
        Pin<Box<dyn Stream<Item = Result<pb::Event, Status>> + Send + 'static>>;

    async fn subscribe(
        &self,
        req: Request<pb::SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let kinds = req.into_inner().kinds;
        let rx = zerod_events::subscribe();
        let stream = BroadcastStream::new(rx).filter_map(move |item| match item {
            Ok(env) => {
                if kinds_match(&kinds, env.event.kind()) {
                    Some(Ok(envelope_to_proto(env)))
                } else {
                    None
                }
            }
            Err(BroadcastStreamRecvError::Lagged(n)) => Some(Ok(lagged_proto(n))),
        });
        Ok(Response::new(Box::pin(stream)))
    }
}

fn kinds_match(filters: &[String], kind: &str) -> bool {
    if filters.is_empty() {
        return true;
    }
    filters.iter().any(|f| {
        if let Some(prefix) = f.strip_suffix(".*") {
            kind.starts_with(prefix)
        } else {
            f == kind
        }
    })
}

fn envelope_to_proto(env: Envelope) -> pb::Event {
    pb::Event {
        timestamp_ms: env.timestamp_ms,
        payload: Some(event_to_payload(env.event)),
    }
}

fn event_to_payload(event: Event) -> pb::event::Payload {
    use pb::event::Payload;
    match event {
        Event::StreamStateChanged { state, url, error } => {
            Payload::StreamState(pb::StreamStateChanged {
                state: stream_state_to_pb(state) as i32,
                url,
                error,
            })
        }
        Event::StreamVolumeChanged { volume_percent } => {
            Payload::StreamVolume(pb::StreamVolumeChanged { volume_percent })
        }
        Event::BluetoothDeviceChanged {
            address,
            name,
            paired,
            connected,
            trusted,
        } => Payload::BtDevice(pb::BluetoothDeviceChanged {
            address,
            name,
            paired,
            connected,
            trusted,
        }),
        Event::BluetoothPairingRequest { address, passkey } => {
            Payload::BtPairingRequest(pb::BluetoothPairingRequest { address, passkey })
        }
        Event::BluetoothA2dpConnected { address, name } => {
            Payload::BtA2dpConnected(pb::BluetoothA2dpConnected { address, name })
        }
        Event::BluetoothA2dpDisconnected { address, name } => {
            Payload::BtA2dpDisconnected(pb::BluetoothA2dpDisconnected { address, name })
        }
        Event::SystemdUnitState {
            name,
            active_state,
            sub_state,
            enabled,
        } => Payload::SystemdUnit(pb::SystemdUnitState {
            name,
            active_state,
            sub_state,
            enabled,
        }),
        Event::VolumeChanged {
            card,
            control,
            volume_percent,
            muted,
        } => Payload::Volume(pb::VolumeChanged {
            card,
            control,
            volume_percent,
            muted,
        }),
        Event::SnapcastClientChanged {
            client_id,
            name,
            volume_percent,
            muted,
        } => Payload::SnapClient(pb::SnapcastClientChanged {
            client_id,
            name,
            volume_percent,
            muted,
        }),
        Event::LibrespotStateChanged { state, track } => {
            Payload::LibrespotState(pb::LibrespotStateChanged { state, track })
        }
    }
}

fn stream_state_to_pb(s: StreamState) -> pb::PlayerState {
    match s {
        StreamState::Stopped => pb::PlayerState::Stopped,
        StreamState::Buffering => pb::PlayerState::Buffering,
        StreamState::Playing => pb::PlayerState::Playing,
        StreamState::Paused => pb::PlayerState::Paused,
        StreamState::Errored => pb::PlayerState::Errored,
    }
}

fn lagged_proto(n: u64) -> pb::Event {
    pb::Event {
        timestamp_ms: 0,
        payload: Some(pb::event::Payload::Lagged(pb::LaggedEvent { dropped: n })),
    }
}
