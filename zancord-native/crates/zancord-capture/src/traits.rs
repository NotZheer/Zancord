//! Platform-agnostic screen capture traits (Phase 3A.1).
//!
//! Implementations: `macos.rs` (ScreenCaptureKit), `linux.rs` (PipeWire + XDG portal).

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSourceType {
    Display,
    Window,
    #[cfg(target_os = "macos")]
    Application,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra,
    Nv12,
}

#[derive(Debug, Clone)]
pub struct CaptureSource {
    pub id: String,
    pub name: String,
    pub source_type: CaptureSourceType,
    pub thumbnail: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub capture_audio: bool,
    pub exclude_self_audio: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            capture_audio: true,
            exclude_self_audio: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CapturedVideoFrame {
    /// BGRA or NV12 depending on platform.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
    pub timestamp_us: u64,
}

#[derive(Debug, Clone)]
pub struct CapturedAudioFrame {
    pub pcm_data: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub timestamp_us: u64,
}

pub trait ScreenCapturer: Send + 'static {
    fn available_sources(&self) -> Result<Vec<CaptureSource>>;
    fn start_capture(&mut self, source: &CaptureSource, config: &CaptureConfig) -> Result<()>;
    fn stop_capture(&mut self) -> Result<()>;
    /// Receiver for captured video frames. Implementations own the channel.
    fn video_frame_rx(&self) -> &std::sync::mpsc::Receiver<CapturedVideoFrame>;
    /// Receiver for captured system audio, if audio capture is enabled.
    fn audio_sample_rx(&self) -> Option<&std::sync::mpsc::Receiver<CapturedAudioFrame>>;
}
