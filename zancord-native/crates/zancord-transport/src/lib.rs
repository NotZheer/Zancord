//! Zancord WebRTC transport: full-mesh peer connections over Tailscale
//! (no STUN/TURN), perfect negotiation, track management, RTP bridge.

#![deny(clippy::all)]

pub mod bridge;
pub mod engine;
pub mod mesh;
pub mod negotiation;
pub mod peer;
pub mod rtcp;
pub mod tracks;
