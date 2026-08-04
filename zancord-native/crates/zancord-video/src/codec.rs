//! Codec abstraction (Phase 3C.6): uniform encoder/decoder interface over
//! H.264 (openh264) and VP8 (vpx).

use zancord_protocol::{EncodedVideoFrame, VideoCodec};

use crate::convert::I420Frame;

#[derive(Debug, Clone, PartialEq)]
pub struct VideoEncoderConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
}

impl VideoEncoderConfig {
    /// Sensible defaults: 720p30 at 800 kbps.
    pub fn new(codec: VideoCodec) -> Self {
        Self {
            codec,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_bps: 800_000,
        }
    }
}

pub trait VideoEncoder: Send + 'static {
    /// Encode one I420 frame.
    fn encode(&mut self, frame: &I420Frame) -> anyhow::Result<EncodedVideoFrame>;
    /// Request an immediate keyframe (on PLI).
    fn force_keyframe(&mut self);
    /// Adjust target bitrate (on REMB / bandwidth estimation).
    fn set_bitrate(&mut self, bitrate_bps: u32);
    /// Recreate the encoder for a new resolution.
    fn set_resolution(&mut self, width: u32, height: u32) -> anyhow::Result<()>;
}

pub trait VideoDecoder: Send + 'static {
    /// Decode one encoded frame. Returns the I420 frame, or `None` while
    /// waiting for a keyframe / after a dropped frame.
    fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<I420Frame>>;
}

/// Create the concrete encoder for `codec`. Implemented in Phase 3C.
pub fn create_encoder(_config: &VideoEncoderConfig) -> anyhow::Result<Box<dyn VideoEncoder>> {
    anyhow::bail!("encoder not implemented until Phase 3C")
}

/// Create the concrete decoder for `codec`. Implemented in Phase 3C.
pub fn create_decoder(_codec: VideoCodec) -> anyhow::Result<Box<dyn VideoDecoder>> {
    anyhow::bail!("decoder not implemented until Phase 3C")
}
