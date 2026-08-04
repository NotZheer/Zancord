//! H.264 decoder via openh264 (Phase 3C.3).
//!
//! Input: Annex-B H.264 bitstream (as produced by [`super::h264_encoder`]).
//! Output: planar I420. The decoder tolerates missing/corrupted frames: it
//! returns `None` until a decodable frame (keyframe) arrives.

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

use crate::convert::I420Frame;

pub struct H264Decoder {
    inner: Decoder,
}

impl Default for H264Decoder {
    fn default() -> Self {
        Self::new().expect("openh264 decoder creation cannot fail")
    }
}

impl H264Decoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            inner: Decoder::new()?,
        })
    }

    /// Decode one access unit. Returns `None` when no frame was produced yet
    /// (e.g. waiting for the first keyframe, or a corrupt packet was skipped).
    ///
    /// The decoded planes are stride-padded by openh264; rows are copied out
    /// individually into a packed I420 frame.
    pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<I420Frame>> {
        let Some(decoded) = self.inner.decode(data)? else {
            return Ok(None);
        };

        let (width, height) = decoded.dimensions();
        let (y_stride, uv_stride, _) = decoded.strides();
        let w = width;
        let h = height;
        let uv_w = w / 2;
        let uv_h = h / 2;

        let mut frame = I420Frame {
            width: width as u32,
            height: height as u32,
            y: vec![0u8; w * h],
            u: vec![0u8; uv_w * uv_h],
            v: vec![0u8; uv_w * uv_h],
        };

        let y = decoded.y();
        let u = decoded.u();
        let v = decoded.v();
        for row in 0..h {
            let src = row * y_stride;
            let dst = row * w;
            frame.y[dst..dst + w].copy_from_slice(&y[src..src + w]);
        }
        for row in 0..uv_h {
            let src = row * uv_stride;
            let dst = row * uv_w;
            frame.u[dst..dst + uv_w].copy_from_slice(&u[src..src + uv_w]);
            frame.v[dst..dst + uv_w].copy_from_slice(&v[src..src + uv_w]);
        }
        Ok(Some(frame))
    }
}
