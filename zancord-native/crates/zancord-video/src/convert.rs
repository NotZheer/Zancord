//! Color space conversion (Phase 3C.1): BGRA/NV12 → I420 for encoders,
//! I420 → RGBA for Slint `SharedPixelBuffer` display.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct I420Frame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum ConversionError {
    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("conversion not implemented yet (Phase 3C.1)")]
    NotImplemented,
}

/// Convert BGRA (premultiplied, 8bpc) to planar I420. Implemented in Phase 3C.1.
pub fn bgra_to_i420(data: &[u8], width: u32, height: u32) -> Result<I420Frame, ConversionError> {
    let _ = (data, width, height);
    Err(ConversionError::NotImplemented)
}

/// Convert NV12 (bi-planar) to planar I420.
pub fn nv12_to_i420(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: u32,
    height: u32,
) -> Result<I420Frame, ConversionError> {
    let _ = (y_plane, uv_plane, width, height);
    Err(ConversionError::NotImplemented)
}

/// Convert planar I420 to RGBA (for UI display).
pub fn i420_to_rgba(frame: &I420Frame) -> Result<Vec<u8>, ConversionError> {
    let _ = frame;
    Err(ConversionError::NotImplemented)
}
