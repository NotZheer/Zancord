//! Shared types and message definitions used across every Zancord crate.
//!
//! This crate has NO dependencies on other Zancord crates and no runtime
//! behavior — it is pure data definitions (protocol, room, media state).

#![deny(clippy::all)]

pub mod media;
pub mod messages;
pub mod peer;
pub mod room;

pub use media::{
    AudioCodec, AudioProcessingConfig, EncodedAudioFrame, EncodedVideoFrame, ScreenShareQuality,
    VideoCodec,
};
pub use messages::SignalMessage;
pub use peer::{PeerId, Username};
pub use room::{MediaStatePayload, PeerInfo, DEFAULT_ROOM, MAX_ROOM_SIZE};
