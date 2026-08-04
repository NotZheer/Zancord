//! Media configuration types shared across capture, audio, video, and transport.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    VP8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AudioCodec {
    Opus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenShareQuality {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl ScreenShareQuality {
    pub fn presets() -> Vec<(&'static str, Self)> {
        vec![
            ("1080p", Self { width: 1920, height: 1080, fps: 30 }),
            ("720p", Self { width: 1280, height: 720, fps: 30 }),
            ("540p", Self { width: 960, height: 540, fps: 30 }),
            ("360p", Self { width: 640, height: 360, fps: 15 }),
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioProcessingConfig {
    pub hpf_enabled: bool,
    pub hpf_cutoff_hz: f32, // default: 80.0
    pub noise_gate_enabled: bool,
    pub noise_gate_threshold_db: f32, // default: -45.0
}

impl Default for AudioProcessingConfig {
    fn default() -> Self {
        Self {
            hpf_enabled: true,
            hpf_cutoff_hz: 80.0,
            noise_gate_enabled: true,
            noise_gate_threshold_db: -45.0,
        }
    }
}

/// One encoded Opus packet, produced by the audio pipeline and consumed by the
/// WebRTC transport (and vice versa on the receive path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodedAudioFrame {
    pub data: Vec<u8>,
    pub sequence: u64,
    pub timestamp_ms: u64,
}

/// One encoded video frame (H.264 Annex-B NAL units or VP8 frame), produced by
/// the video pipeline and consumed by the WebRTC transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodedVideoFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub timestamp_ms: u64,
}
