//! Error types for the audio crate (`thiserror` enum, per AGENT.md).

use thiserror::Error;

/// Errors produced by the audio crate.
#[derive(Debug, Error)]
pub enum AudioError {
    #[error("no such audio device: {0}")]
    NoDevice(String),

    #[error("failed to build audio stream: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),

    #[error("failed to read default stream config: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),

    #[error("failed to start stream: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),

    #[error("audio stream error: {0}")]
    Stream(#[from] cpal::StreamError),

    #[error("failed to enumerate devices: {0}")]
    Devices(#[from] cpal::DevicesError),

    #[error("failed to read device name: {0}")]
    DeviceName(#[from] cpal::DeviceNameError),

    #[error("failed to enumerate supported stream configs: {0}")]
    SupportedConfigs(#[from] cpal::SupportedStreamConfigsError),

    #[error("opus codec error: {0}")]
    Codec(#[from] opus::Error),

    #[error("resampler construction error: {0}")]
    ResampleConstruction(#[from] rubato::ResamplerConstructionError),

    #[error("resampling error: {0}")]
    Resample(#[from] rubato::ResampleError),

    #[error("failed to spawn audio thread: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("invalid audio configuration: {0}")]
    Config(String),
}

/// Convenience result alias for the whole crate.
pub type Result<T> = std::result::Result<T, AudioError>;
