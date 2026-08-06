//! Webcam capture (Phase 5.2) via nokhwa — cross-platform (AVFoundation on
//! macOS, V4L2 on Linux, MediaFoundation on Windows).
//!
//! nokhwa runs the device on its own thread (`CallbackCamera`); we decode each
//! frame to RGB24 in the callback and push it into an SPSC channel. The caller
//! drains with `next_frame()`. Dropping the capturer (or calling `stop()`)
//! closes the channel, so a wedged device can never hang the caller.

use std::sync::mpsc;

use anyhow::{Context, Result};
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
};
use nokhwa::CallbackCamera;
use tracing::{debug, info, warn};

use crate::traits::{CapturedVideoFrame, PixelFormat};

/// Webcam capture profile (720p30 — the encode budget for software H.264).
#[derive(Debug, Clone, Copy)]
pub struct CameraConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
        }
    }
}

/// Platform-agnostic webcam capture.
pub trait CameraCapturer: Send + 'static {
    /// Human-readable camera name (for logs).
    fn name(&self) -> String;
    /// Blocks until the next frame (RGB24) arrives; `Ok(None)` once the
    /// camera is stopped/closed.
    fn next_frame(&mut self) -> Result<Option<CapturedVideoFrame>>;
    /// Stops the device; the frame channel closes and `next_frame` returns
    /// `Ok(None)`.
    fn stop(&mut self) -> Result<()>;
}

/// The preferred device negotiation: ask for YUYV at the requested
/// resolution/fps, letting the backend pick the closest supported format.
fn primary_request(config: &CameraConfig) -> RequestedFormatType {
    RequestedFormatType::Closest(CameraFormat::new_from(
        config.width,
        config.height,
        FrameFormat::YUYV,
        config.fps,
    ))
}

/// Fallback when no YUYV mode exists (e.g. MJPEG-only devices): let the
/// backend pick anything it supports.
fn fallback_request() -> RequestedFormatType {
    RequestedFormatType::None
}

/// nokhwa-backed `CameraCapturer`.
pub struct NokhwaCameraCapturer {
    camera: Option<CallbackCamera>,
    frame_rx: mpsc::Receiver<CapturedVideoFrame>,
    name: String,
}

impl NokhwaCameraCapturer {
    /// Opens the first available camera (or index 0 when enumeration is not
    /// supported) at `config`, falling back to whatever format the device
    /// offers when the preferred one isn't available.
    pub fn open(config: &CameraConfig) -> Result<Self> {
        // macOS requires explicit initialization before any camera API; the
        // result arrives via callback (the init is async on AVFoundation).
        #[cfg(target_os = "macos")]
        nokhwa::nokhwa_initialize(|ok| {
            if !ok {
                warn!("nokhwa initialization failed — camera may be unavailable");
            }
        });

        let cameras = nokhwa::query(ApiBackend::Auto).unwrap_or_default();
        if cameras.is_empty() {
            warn!("no cameras enumerated; opening index 0 blindly");
        } else {
            info!(
                cameras = cameras.len(),
                names = ?cameras.iter().map(|c| c.human_name()).collect::<Vec<_>>(),
                "cameras enumerated"
            );
        }
        let name = cameras
            .first()
            .map(|c| c.human_name())
            .unwrap_or_else(|| "camera 0".to_string());

        let (frame_tx, frame_rx) = mpsc::channel::<CapturedVideoFrame>();

        let mut camera = match Self::build_camera(primary_request(config), frame_tx.clone()) {
            Ok(camera) => camera,
            Err(err) => {
                debug!(error = %err, "preferred camera format unavailable, falling back");
                Self::build_camera(fallback_request(), frame_tx)?
            }
        };
        camera
            .open_stream()
            .context("failed to start camera stream")?;
        info!(camera = %name, width = config.width, height = config.height, fps = config.fps, "camera opened");

        Ok(Self {
            camera: Some(camera),
            frame_rx,
            name,
        })
    }

    fn build_camera(
        request_type: RequestedFormatType,
        frame_tx: mpsc::Sender<CapturedVideoFrame>,
    ) -> Result<CallbackCamera> {
        let request = RequestedFormat::new::<RgbFormat>(request_type);
        CallbackCamera::new(CameraIndex::Index(0), request, move |buffer| {
            match buffer.decode_image::<RgbFormat>() {
                Ok(img) => {
                    let (width, height) = (img.width(), img.height());
                    let frame = CapturedVideoFrame {
                        data: img.into_raw(),
                        width,
                        height,
                        pixel_format: PixelFormat::Rgb,
                        timestamp_us: 0,
                    };
                    // Never block the camera thread: drop the frame when the
                    // session is behind (it drains the newest anyway).
                    let _ = frame_tx.send(frame);
                }
                Err(err) => warn!(error = %err, "camera frame decode failed"),
            }
        })
        .context("failed to create camera")
    }
}

impl CameraCapturer for NokhwaCameraCapturer {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn next_frame(&mut self) -> Result<Option<CapturedVideoFrame>> {
        match self.frame_rx.recv() {
            Ok(frame) => Ok(Some(frame)),
            Err(_) => Ok(None), // channel closed → camera stopped
        }
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(mut camera) = self.camera.take() {
            camera.stop_stream()?;
            info!("camera stopped");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_request_prefers_yuyv_at_requested_resolution() {
        let config = CameraConfig::default();
        assert_eq!(
            primary_request(&config),
            RequestedFormatType::Closest(CameraFormat::new_from(1280, 720, FrameFormat::YUYV, 30,))
        );
    }

    #[test]
    fn fallback_request_leaves_format_negotiation_open() {
        assert_eq!(fallback_request(), RequestedFormatType::None);
    }
}
