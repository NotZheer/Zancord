//! H.264 encoder via openh264 (Phase 3C.2).
//!
//! Input: planar I420. Output: Annex-B H.264 bitstream (SPS/PPS + NAL units).
//! Resolution changes are handled by openh264 itself (it re-initializes when
//! the input dimensions change).

use openh264::encoder::{Encoder, EncoderConfig, FrameType};
use openh264::formats::YUVBuffer;

use zancord_protocol::EncodedVideoFrame;

use crate::convert::I420Frame;

pub struct H264Encoder {
    inner: Encoder,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_bps: u32,
    keyframe_requested: bool,
}

impl H264Encoder {
    pub fn new(width: u32, height: u32, fps: u32, bitrate_bps: u32) -> anyhow::Result<Self> {
        let config = EncoderConfig::new()
            .set_bitrate_bps(bitrate_bps)
            .max_frame_rate(fps as f32)
            .enable_skip_frame(false); // never skip frames in a call
        let inner = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)?;
        Ok(Self {
            inner,
            width,
            height,
            fps,
            bitrate_bps,
            keyframe_requested: false,
        })
    }

    pub fn encode_frame(&mut self, frame: &I420Frame) -> anyhow::Result<EncodedVideoFrame> {
        if frame.width != self.width || frame.height != self.height {
            self.width = frame.width;
            self.height = frame.height;
            // openh264 re-initializes on dimension change; request an IDR so
            // the resolution switch is clean for the receiver.
            self.keyframe_requested = true;
        }

        let mut packed = Vec::with_capacity(frame.y.len() + frame.u.len() + frame.v.len());
        packed.extend_from_slice(&frame.y);
        packed.extend_from_slice(&frame.u);
        packed.extend_from_slice(&frame.v);
        let yuv = YUVBuffer::from_vec(packed, self.width as usize, self.height as usize);

        if self.keyframe_requested {
            self.inner.force_intra_frame();
            self.keyframe_requested = false;
        }
        let bitstream = self.inner.encode(&yuv)?;
        let keyframe = bitstream.frame_type() == FrameType::IDR;

        Ok(EncodedVideoFrame {
            data: bitstream.to_vec(),
            keyframe,
            width: self.width,
            height: self.height,
            fps: self.fps,
            timestamp_ms: 0,
        })
    }

    /// Request an immediate IDR on the next frame (on PLI).
    pub fn force_keyframe(&mut self) {
        self.keyframe_requested = true;
    }

    /// Record a new target bitrate. openh264 applies it on the next encoder
    /// re-initialization (resolution change or recreate).
    pub fn set_bitrate(&mut self, bitrate_bps: u32) {
        self.bitrate_bps = bitrate_bps;
    }

    pub fn bitrate_bps(&self) -> u32 {
        self.bitrate_bps
    }
}
