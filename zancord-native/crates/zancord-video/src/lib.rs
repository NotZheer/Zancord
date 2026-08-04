//! Zancord video: color conversion, H.264/VP8 encode/decode, codec abstraction.

#![deny(clippy::all)]

pub mod codec;
pub mod convert;
pub mod h264_decoder;
pub mod h264_encoder;
pub mod vp8_decoder;
pub mod vp8_encoder;

pub use codec::{VideoDecoder, VideoEncoder, VideoEncoderConfig};
pub use convert::{ConversionError, I420Frame};
