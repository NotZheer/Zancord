//! Codec abstraction (Phase 3C.6): uniform encoder/decoder interface over
//! H.264 (openh264) and VP8 (vpx).

use zancord_protocol::{EncodedVideoFrame, VideoCodec};

use crate::convert::I420Frame;
use crate::h264_decoder::H264Decoder;
use crate::h264_encoder::H264Encoder;

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

/// Create the concrete encoder for `codec`.
pub fn create_encoder(config: &VideoEncoderConfig) -> anyhow::Result<Box<dyn VideoEncoder>> {
    match config.codec {
        VideoCodec::H264 => Ok(Box::new(H264Encoder::new(
            config.width,
            config.height,
            config.fps,
            config.bitrate_bps,
        )?)),
        VideoCodec::VP8 => {
            anyhow::bail!(
                "VP8 encoder unavailable: the vpx crate is unusable (codec interfaces disabled)"
            )
        }
    }
}

/// Create the concrete decoder for `codec`.
pub fn create_decoder(codec: VideoCodec) -> anyhow::Result<Box<dyn VideoDecoder>> {
    match codec {
        VideoCodec::H264 => Ok(Box::new(H264Decoder::new()?)),
        VideoCodec::VP8 => {
            anyhow::bail!(
                "VP8 decoder unavailable: the vpx crate is unusable (codec interfaces disabled)"
            )
        }
    }
}

impl VideoEncoder for H264Encoder {
    fn encode(&mut self, frame: &I420Frame) -> anyhow::Result<EncodedVideoFrame> {
        self.encode_frame(frame)
    }

    fn force_keyframe(&mut self) {
        self.force_keyframe()
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) {
        self.set_bitrate(bitrate_bps)
    }

    fn set_resolution(&mut self, width: u32, height: u32) -> anyhow::Result<()> {
        // openh264 re-initializes on the next encode when dimensions change.
        let _ = (width, height);
        Ok(())
    }
}

impl VideoDecoder for H264Decoder {
    fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<I420Frame>> {
        self.decode(data)
    }
}
