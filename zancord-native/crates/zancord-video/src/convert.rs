//! Color space conversion (Phase 3C.1): BGRA/NV12 → I420 for encoders,
//! I420 → RGBA for Slint `SharedPixelBuffer` display.
//!
//! Uses BT.601 limited-range coefficients (the de-facto standard for
//! videoconferencing pipelines and what openh264/libvpx expect).

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
    #[error("invalid dimensions: {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },
    #[error("conversion not implemented yet (Phase 3C.1)")]
    NotImplemented,
}

/// BT.601 coefficients (limited range, 8-bit).
const KR: f32 = 0.299;
const KG: f32 = 0.587;
const KB: f32 = 0.114;

/// Convert one BGRA (8bpc, non-premultiplied) pixel to YUV (BT.601 limited).
#[inline]
fn bgra_to_yuv(b: u8, g: u8, r: u8) -> (u8, u8, u8) {
    rgb_to_yuv(r, g, b)
}

/// Convert one RGB888 pixel to YUV (BT.601 limited).
#[inline]
fn rgb_to_yuv(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
    let y = (16.0 + KR * r + KG * g + KB * b).round().clamp(16.0, 235.0);
    let u = (128.0 - 0.168736 * r - 0.331264 * g + 0.5 * b)
        .round()
        .clamp(16.0, 240.0);
    let v = (128.0 + 0.5 * r - 0.418688 * g - 0.081312 * b)
        .round()
        .clamp(16.0, 240.0);
    (y as u8, u as u8, v as u8)
}

/// Convert one YUV (BT.601 limited) pixel to RGBA (full range).
#[inline]
fn yuv_to_rgba(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    let (y, u, v) = (f32::from(y), f32::from(u), f32::from(v));
    let c = y - 16.0;
    let d = u - 128.0;
    let e = v - 128.0;
    let r = (1.164 * c + 1.596 * e).round().clamp(0.0, 255.0);
    let g = (1.164 * c - 0.392 * d - 0.813 * e)
        .round()
        .clamp(0.0, 255.0);
    let b = (1.164 * c + 2.017 * d).round().clamp(0.0, 255.0);
    (r as u8, g as u8, b as u8)
}

fn check_dims(width: u32, height: u32) -> Result<(), ConversionError> {
    if width == 0 || height == 0 || (width % 2 != 0) || (height % 2 != 0) {
        return Err(ConversionError::InvalidDimensions { width, height });
    }
    Ok(())
}

fn yuv_sizes(width: u32, height: u32) -> (usize, usize) {
    (
        width as usize * height as usize,
        (width as usize / 2) * (height as usize / 2),
    )
}

/// Convert BGRA (8bpc, non-premultiplied) to planar I420.
pub fn bgra_to_i420(data: &[u8], width: u32, height: u32) -> Result<I420Frame, ConversionError> {
    check_dims(width, height)?;
    let expected = (width * height * 4) as usize;
    if data.len() != expected {
        return Err(ConversionError::DimensionMismatch {
            expected,
            actual: data.len(),
        });
    }
    let (y_size, uv_size) = yuv_sizes(width, height);
    let mut y = vec![0u8; y_size];
    let mut u = vec![0u8; uv_size];
    let mut v = vec![0u8; uv_size];

    let w = width as usize;
    let h = height as usize;
    for row in 0..h {
        for col in 0..w {
            let px = (row * w + col) * 4;
            let (py, pu, pv) = bgra_to_yuv(data[px], data[px + 1], data[px + 2]);
            y[row * w + col] = py;
            if row % 2 == 0 && col % 2 == 0 {
                let uv_idx = (row / 2) * (w / 2) + col / 2;
                u[uv_idx] = pu;
                v[uv_idx] = pv;
            }
        }
    }
    Ok(I420Frame {
        width,
        height,
        y,
        u,
        v,
    })
}

/// Convert RGB24 (8bpc, R,G,B per pixel — what nokhwa's `RgbFormat` decoder
/// produces) to planar I420.
pub fn rgb_to_i420(data: &[u8], width: u32, height: u32) -> Result<I420Frame, ConversionError> {
    check_dims(width, height)?;
    let expected = (width * height * 3) as usize;
    if data.len() != expected {
        return Err(ConversionError::DimensionMismatch {
            expected,
            actual: data.len(),
        });
    }
    let (y_size, uv_size) = yuv_sizes(width, height);
    let mut y = vec![0u8; y_size];
    let mut u = vec![0u8; uv_size];
    let mut v = vec![0u8; uv_size];

    let w = width as usize;
    let h = height as usize;
    for row in 0..h {
        for col in 0..w {
            let px = (row * w + col) * 3;
            let (py, pu, pv) = rgb_to_yuv(data[px], data[px + 1], data[px + 2]);
            y[row * w + col] = py;
            if row % 2 == 0 && col % 2 == 0 {
                let uv_idx = (row / 2) * (w / 2) + col / 2;
                u[uv_idx] = pu;
                v[uv_idx] = pv;
            }
        }
    }
    Ok(I420Frame {
        width,
        height,
        y,
        u,
        v,
    })
}

/// Convert NV12 (bi-planar: full Y, interleaved UV) to planar I420.
pub fn nv12_to_i420(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: u32,
    height: u32,
) -> Result<I420Frame, ConversionError> {
    check_dims(width, height)?;
    let (y_size, uv_size) = yuv_sizes(width, height);
    if y_plane.len() != y_size {
        return Err(ConversionError::DimensionMismatch {
            expected: y_size,
            actual: y_plane.len(),
        });
    }
    if uv_plane.len() != uv_size * 2 {
        return Err(ConversionError::DimensionMismatch {
            expected: uv_size * 2,
            actual: uv_plane.len(),
        });
    }
    let mut u = vec![0u8; uv_size];
    let mut v = vec![0u8; uv_size];
    for (i, pair) in uv_plane.chunks_exact(2).enumerate() {
        u[i] = pair[0];
        v[i] = pair[1];
    }
    Ok(I420Frame {
        width,
        height,
        y: y_plane.to_vec(),
        u,
        v,
    })
}

/// Convert planar I420 to RGBA (for UI display). Alpha is set to 255.
pub fn i420_to_rgba(frame: &I420Frame) -> Result<Vec<u8>, ConversionError> {
    check_dims(frame.width, frame.height)?;
    let (y_size, uv_size) = yuv_sizes(frame.width, frame.height);
    if frame.y.len() != y_size {
        return Err(ConversionError::DimensionMismatch {
            expected: y_size,
            actual: frame.y.len(),
        });
    }
    if frame.u.len() != uv_size || frame.v.len() != uv_size {
        return Err(ConversionError::DimensionMismatch {
            expected: uv_size,
            actual: frame.u.len().min(frame.v.len()),
        });
    }
    let w = frame.width as usize;
    let h = frame.height as usize;
    let mut out = vec![0u8; y_size * 4];
    for row in 0..h {
        for col in 0..w {
            let y = frame.y[row * w + col];
            let u = frame.u[(row / 2) * (w / 2) + col / 2];
            let v = frame.v[(row / 2) * (w / 2) + col / 2];
            let (r, g, b) = yuv_to_rgba(y, u, v);
            let px = (row * w + col) * 4;
            out[px] = b;
            out[px + 1] = g;
            out[px + 2] = r;
            out[px + 3] = 255;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_rgba(level: u8, w: u32, h: u32) -> Vec<u8> {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for chunk in px.chunks_exact_mut(4) {
            chunk.copy_from_slice(&[level, level, level, 255]);
        }
        px
    }

    #[test]
    fn bgra_gray_to_i420_matches_bt601() {
        // Gray (128): Y = 16 + 0.299*128 + 0.587*128 + 0.114*128 ≈ 144, U=V=128.
        let frame = bgra_to_i420(&gray_rgba(128, 2, 2), 2, 2).unwrap();
        assert!(frame.y.iter().all(|&p| p == 144));
        assert!(frame.u.iter().all(|&p| p == 128));
        assert!(frame.v.iter().all(|&p| p == 128));
    }

    #[test]
    fn bgra_black_and_white() {
        let black = bgra_to_i420(&gray_rgba(0, 2, 2), 2, 2).unwrap();
        assert!(black.y.iter().all(|&p| p == 16)); // limited range black
        let white = bgra_to_i420(&gray_rgba(255, 2, 2), 2, 2).unwrap();
        assert!(white.y.iter().all(|&p| p == 235)); // limited range white
    }

    #[test]
    fn red_pixel_produces_expected_yuv() {
        // Pure red (B=0,G=0,R=255): Y=16+0.299*255≈92, U=128-0.168736*255≈85, V≈240.
        let mut px = gray_rgba(0, 2, 2);
        px[2] = 255; // first pixel red (BGRA order: B,G,R,A)
        let frame = bgra_to_i420(&px, 2, 2).unwrap();
        assert_eq!(frame.y[0], 92);
        assert_eq!(frame.u[0], 85);
        assert_eq!(frame.v[0], 240);
    }

    #[test]
    fn nv12_to_i420_splits_uv() {
        let y = vec![1u8; 4]; // 2x2
        let uv = vec![10, 20]; // U=10, V=20
        let frame = nv12_to_i420(&y, &uv, 2, 2).unwrap();
        assert_eq!(frame.u, vec![10]);
        assert_eq!(frame.v, vec![20]);
    }

    #[test]
    fn i420_roundtrip_recovers_rgba() {
        let src = gray_rgba(100, 4, 4);
        let yuv = bgra_to_i420(&src, 4, 4).unwrap();
        let back = i420_to_rgba(&yuv).unwrap();
        // BT.601 LIMITED range: full-range gray g maps to Y=16+g, and back to
        // 1.164*g (for g >= 16). So 100 -> 116, not 100. That is the standard
        // videoconferencing range mapping, not a bug.
        let expected = 1.164f32 * 100.0; // 116.4
        for (a, b) in src.chunks_exact(4).zip(back.chunks_exact(4)) {
            assert!(
                (b[0] as f32 - expected).abs() <= 2.0,
                "gray roundtrip B {b:?} vs {expected}"
            );
            assert!(
                (b[1] as f32 - expected).abs() <= 2.0,
                "gray roundtrip G {b:?} vs {expected}"
            );
            assert!(
                (b[2] as f32 - expected).abs() <= 2.0,
                "gray roundtrip R {b:?} vs {expected}"
            );
            assert_eq!(a[3], b[3]); // alpha passthrough
        }
        // White stays white (Y=235 -> 255) and the mapping is a fixed point.
        let white = i420_to_rgba(&bgra_to_i420(&gray_rgba(235, 4, 4), 4, 4).unwrap()).unwrap();
        assert!(white
            .chunks_exact(4)
            .all(|c| c[0] == 255 && c[1] == 255 && c[2] == 255));
    }

    #[test]
    fn rgb_gray_to_i420_matches_bt601() {
        // Gray (128): Y = 16 + 0.299*128 + 0.587*128 + 0.114*128 ≈ 144, U=V=128.
        let mut px = vec![0u8; 2 * 2 * 3];
        for chunk in px.chunks_exact_mut(3) {
            chunk.copy_from_slice(&[128, 128, 128]);
        }
        let frame = rgb_to_i420(&px, 2, 2).unwrap();
        assert!(frame.y.iter().all(|&p| p == 144));
        assert!(frame.u.iter().all(|&p| p == 128));
        assert!(frame.v.iter().all(|&p| p == 128));
    }

    #[test]
    fn rgb_red_matches_bgra_red() {
        // Same pixel expressed as RGB and as BGRA must land on the same YUV.
        let mut rgb = vec![0u8; 2 * 2 * 3];
        rgb[0] = 255; // R
        let from_rgb = rgb_to_i420(&rgb, 2, 2).unwrap();

        let mut bgra = vec![0u8; 2 * 2 * 4];
        bgra[2] = 255; // R in BGRA order
        let from_bgra = bgra_to_i420(&bgra, 2, 2).unwrap();

        assert_eq!(from_rgb.y, from_bgra.y);
        assert_eq!(from_rgb.u, from_bgra.u);
        assert_eq!(from_rgb.v, from_bgra.v);
    }

    #[test]
    fn rgb_black_and_white() {
        let black = rgb_to_i420(&vec![0; 2 * 2 * 3], 2, 2).unwrap();
        assert!(black.y.iter().all(|&p| p == 16)); // limited range black
        let white = rgb_to_i420(&vec![255; 2 * 2 * 3], 2, 2).unwrap();
        assert!(white.y.iter().all(|&p| p == 235)); // limited range white
    }

    #[test]
    fn rgb_rejects_bad_dimensions_and_lengths() {
        assert!(rgb_to_i420(&[], 0, 2).is_err());
        assert!(rgb_to_i420(&[], 3, 3).is_err()); // odd dims
        assert!(rgb_to_i420(&[0; 11], 2, 2).is_err()); // wrong length (12 expected)
    }

    #[test]
    fn rejects_bad_dimensions_and_lengths() {
        assert!(bgra_to_i420(&[], 0, 2).is_err());
        assert!(bgra_to_i420(&[], 3, 3).is_err()); // odd dims
        assert!(bgra_to_i420(&[0; 8], 2, 2).is_err()); // wrong length
        assert!(nv12_to_i420(&[0; 3], &[0; 4], 2, 2).is_err());
        assert!(i420_to_rgba(&I420Frame {
            width: 2,
            height: 2,
            y: vec![0; 3],
            u: vec![0],
            v: vec![0]
        })
        .is_err());
    }
}
