//! Resampler (Phase 1C.3): device rate ↔ 48000 Hz via `rubato` `SincFixedIn`,
//! with stereo→mono downmix for voice on the capture side.

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub use crate::codec::{FRAME_SIZE, SAMPLE_RATE};
use crate::error::{AudioError, Result};

/// Max ratio swing we ever ask for (covers 8 kHz..=192 kHz devices).
const MAX_RELATIVE_RATIO: f64 = 8.0;

fn sinc_parameters() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        oversampling_factor: 256,
        interpolation: SincInterpolationType::Nearest,
        window: WindowFunction::BlackmanHarris2,
    }
}

/// Downmix interleaved device samples to mono (linear average of all
/// channels — the voice downmix used on the capture path).
pub fn downmix_to_mono_into(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    for frame in interleaved.chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        out.push(sum / channels as f32);
    }
}

/// Capture-side resampler: device rate (interleaved, N channels) → 48 kHz mono.
///
/// `rubato::SincFixedIn` consumes fixed 960-sample input chunks and produces a
/// varying number of output samples per chunk; complete 48 kHz frames are
/// buffered until [`CaptureResampler::take_frame`] drains them.
pub struct CaptureResampler {
    inner: SincFixedIn<f32>,
    channels: usize,
    mono_in: Vec<f32>,
    mono_scratch: Vec<f32>,
    pending: Vec<f32>,
    out_buf: Vec<f32>,
}

impl CaptureResampler {
    /// New capture resampler for a device running at `device_rate` with
    /// `channels` interleaved channels.
    pub fn new(device_rate: u32, channels: usize) -> Result<Self> {
        if device_rate == 0 || channels == 0 {
            return Err(AudioError::Config(format!(
                "invalid device rate {device_rate} Hz / {channels} channels"
            )));
        }
        let ratio = f64::from(SAMPLE_RATE) / f64::from(device_rate);
        let inner = SincFixedIn::new(ratio, MAX_RELATIVE_RATIO, sinc_parameters(), FRAME_SIZE, 1)?;
        let out_len = inner.output_frames_next();
        Ok(Self {
            inner,
            channels,
            mono_in: Vec::with_capacity(FRAME_SIZE * 2),
            mono_scratch: Vec::with_capacity(FRAME_SIZE * 2),
            pending: Vec::with_capacity(FRAME_SIZE * 2),
            out_buf: vec![0.0; out_len],
        })
    }

    /// Feed interleaved device-rate samples: downmix to mono, accumulate in the
    /// input buffer, then resample complete 960-sample chunks.
    ///
    /// `mono_in` must ACCUMULATE across calls (the device delivers far fewer
    /// than 960 samples per callback); never reset it per push.
    pub fn push(&mut self, interleaved: &[f32]) -> Result<()> {
        if self.channels <= 1 {
            self.mono_in.extend_from_slice(interleaved);
        } else {
            downmix_to_mono_into(interleaved, self.channels, &mut self.mono_scratch);
            self.mono_in.extend_from_slice(&self.mono_scratch);
        }
        while self.mono_in.len() >= FRAME_SIZE {
            let input = [&self.mono_in[..FRAME_SIZE]];
            let mut output = [self.out_buf.as_mut_slice()];
            let (consumed, produced) = self.inner.process_into_buffer(&input, &mut output, None)?;
            self.pending.extend_from_slice(&self.out_buf[..produced]);
            self.mono_in.drain(..consumed);
        }
        Ok(())
    }

    /// Number of complete 48 kHz mono frames currently available.
    pub fn frames_ready(&self) -> usize {
        self.pending.len() / FRAME_SIZE
    }

    /// Pop one complete 48 kHz mono frame, if available.
    pub fn take_frame(&mut self, out: &mut [f32]) -> bool {
        if out.len() < FRAME_SIZE || self.pending.len() < FRAME_SIZE {
            return false;
        }
        out[..FRAME_SIZE].copy_from_slice(&self.pending[..FRAME_SIZE]);
        self.pending.drain(..FRAME_SIZE);
        true
    }

    /// 48 kHz mono samples buffered, including any partial frame.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Device-rate mono samples still waiting for a full 960-sample chunk.
    pub fn input_buffered(&self) -> usize {
        self.mono_in.len()
    }
}

/// Playback-side resampler: 48 kHz mono → device rate mono.
pub struct PlaybackResampler {
    inner: SincFixedIn<f32>,
    out_buf: Vec<f32>,
}

impl PlaybackResampler {
    /// New playback resampler for a device running at `device_rate`.
    pub fn new(device_rate: u32) -> Result<Self> {
        if device_rate == 0 {
            return Err(AudioError::Config(format!(
                "invalid device rate {device_rate} Hz"
            )));
        }
        let ratio = f64::from(device_rate) / f64::from(SAMPLE_RATE);
        let inner = SincFixedIn::new(ratio, MAX_RELATIVE_RATIO, sinc_parameters(), FRAME_SIZE, 1)?;
        let out_len = inner.output_frames_next();
        Ok(Self {
            inner,
            out_buf: vec![0.0; out_len],
        })
    }

    /// Resample one complete 48 kHz mono frame; returns the produced
    /// device-rate samples, borrowed until the next call.
    pub fn process(&mut self, frame: &[f32]) -> Result<&[f32]> {
        if frame.len() != FRAME_SIZE {
            return Err(AudioError::Config(format!(
                "playback resampler expects {FRAME_SIZE} samples, got {}",
                frame.len()
            )));
        }
        let input = [frame];
        let mut output = [self.out_buf.as_mut_slice()];
        let (_consumed, produced) = self.inner.process_into_buffer(&input, &mut output, None)?;
        Ok(&self.out_buf[..produced])
    }
}
