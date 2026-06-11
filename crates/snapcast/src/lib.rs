//! Snapcast (snapserver) JSON-RPC 2.0 client over TCP.
//!
//! One long-lived connection per [`SnapcastClient`], with backoff reconnect.
//! Server-push notifications are forwarded to the in-process event bus when
//! `forward_notifications = true` so external `snapctl` changes don't go
//! invisible to subscribers.

mod client;
mod types;

pub use client::SnapcastClient;
pub use types::{
    SnapClient, SnapClientConfig, SnapGroup, SnapHost, SnapServer, SnapStream, SnapVolume,
};
