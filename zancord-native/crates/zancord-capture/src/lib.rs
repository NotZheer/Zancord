//! Zancord screen + system audio capture.
//!
//! Platform-agnostic traits in `traits.rs`; per-platform backends gated by
//! `#[cfg(target_os = ...)]`:
//! - macOS: ScreenCaptureKit
//! - Linux: PipeWire + XDG Desktop Portal
//! - Camera (all platforms): nokhwa (AVFoundation / V4L2 / MediaFoundation)

#![deny(clippy::all)]

pub mod camera;
pub mod traits;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod linux_audio;
#[cfg(target_os = "macos")]
pub mod macos;

pub use camera::{
    available_cameras, CameraCapturer, CameraConfig, CameraSource, NokhwaCameraCapturer,
};
pub use traits::{
    CaptureConfig, CaptureSource, CaptureSourceType, CapturedAudioFrame, CapturedVideoFrame,
    PixelFormat, ScreenCapturer,
};

/// Creates the platform screen capturer.
#[cfg(target_os = "macos")]
pub fn create_capturer() -> anyhow::Result<Box<dyn ScreenCapturer>> {
    Ok(Box::new(crate::macos::MacScreenCapturer::new()))
}

/// Creates the platform screen capturer.
#[cfg(target_os = "linux")]
pub fn create_capturer() -> anyhow::Result<Box<dyn ScreenCapturer>> {
    Ok(Box::new(crate::linux::LinuxScreenCapturer::new()))
}

/// Creates the platform screen capturer.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn create_capturer() -> anyhow::Result<Box<dyn ScreenCapturer>> {
    anyhow::bail!("screen capture is not supported on this platform")
}

/// Opens the webcam (nokhwa; all platforms).
pub fn create_camera(config: &CameraConfig) -> anyhow::Result<Box<dyn CameraCapturer>> {
    Ok(Box::new(NokhwaCameraCapturer::open(config)?))
}
