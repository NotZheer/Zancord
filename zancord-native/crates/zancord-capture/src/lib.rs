//! Zancord screen + system audio capture.
//!
//! Platform-agnostic traits in `traits.rs`; per-platform backends gated by
//! `#[cfg(target_os = ...)]`:
//! - macOS: ScreenCaptureKit
//! - Linux: PipeWire + XDG Desktop Portal

#![deny(clippy::all)]

pub mod traits;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod linux_audio;
#[cfg(target_os = "macos")]
pub mod macos;

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
