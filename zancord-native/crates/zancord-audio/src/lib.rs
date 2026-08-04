//! Zancord audio: mic capture, playback, resampling, Opus codec, HPF + noise
//! gate processing, and the pipeline orchestrator that ties them together.
//!
//! REAL-TIME SAFETY: cpal callbacks run on real-time OS threads — they may only
//! push/pop lock-free `rtrb` buffers. All processing happens on a dedicated
//! worker thread (`pipeline.rs`).

#![deny(clippy::all)]

pub mod capture;
pub mod codec;
pub mod devices;
pub mod error;
pub mod pipeline;
pub mod playback;
pub mod processor;
pub mod resampler;

pub use error::{AudioError, Result};
