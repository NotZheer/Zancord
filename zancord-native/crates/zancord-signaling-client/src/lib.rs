//! Zancord signaling client library.
//!
//! WebSocket client with auto-reconnect (tokio-tungstenite) that emits parsed
//! `SignalMessage`s over an mpsc channel.

#![deny(clippy::all)]

pub mod client;

pub use client::{ConnectionState, SignalingClient};
