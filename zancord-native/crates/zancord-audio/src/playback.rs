//! Audio playback via cpal (Phase 1C.2): pull mixed remote audio from an
//! `rtrb::Consumer`, zero-fill on underflow, per-peer volume + deafen.
//!
//! The output callback is RT-safe: it only pops the lock-free ring, zero-fills
//! on underflow, expands mono to the device's channel layout, and converts to
//! the device's sample format. Per-peer volumes and deafen live in [`Mixer`],
//! which is only touched by the audio worker thread (`pipeline.rs`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Data, FromSample, OutputCallbackInfo, Sample, SampleFormat, SizedSample, StreamError};
use rtrb::{Consumer, Producer, RingBuffer};
use zancord_protocol::PeerId;

use crate::error::{AudioError, Result};

/// Default playback ring capacity: 1 second of device-rate mono samples.
pub const DEFAULT_RING_CAPACITY: usize = 48_000;

/// Per-peer volume + deafen state, applied on the audio worker thread.
#[derive(Debug, Default)]
pub struct Mixer {
    peer_volumes: HashMap<PeerId, f32>,
    deafened: bool,
}

impl Mixer {
    /// Set a peer's volume, clamped to `0.0..=2.0` (200%). Unset peers are 1.0.
    pub fn set_peer_volume(&mut self, peer: PeerId, volume: f32) {
        self.peer_volumes.insert(peer, volume.clamp(0.0, 2.0));
    }

    /// Forget a peer's volume (falls back to 1.0).
    pub fn remove_peer(&mut self, peer: &PeerId) {
        self.peer_volumes.remove(peer);
    }

    /// Volume for a peer, or 1.0 if unset.
    pub fn peer_volume(&self, peer: &PeerId) -> f32 {
        self.peer_volumes.get(peer).copied().unwrap_or(1.0)
    }

    /// Deafen: mute all playback (global gain 0.0).
    pub fn set_deafened(&mut self, deafened: bool) {
        self.deafened = deafened;
    }

    /// Whether playback is currently deafened.
    pub fn is_deafened(&self) -> bool {
        self.deafened
    }

    /// Apply the global (deafen) gain to an already-mixed mono frame.
    pub fn apply_global_gain(&self, frame: &mut [f32]) {
        if self.deafened {
            frame.fill(0.0);
        }
    }
}

/// Playback stream plus the worker-side ring producer.
///
/// The producer is filled by the audio worker thread with resampled mono; the
/// consumer lives inside the cpal callback and is never touched elsewhere.
pub struct Playback {
    producer: Producer<f32>,
    // Kept alive for the stream's lifetime; dropping it stops playback.
    #[allow(dead_code)]
    stream: Option<cpal::Stream>,
    sample_rate: u32,
    channels: u16,
    mixer: Mixer,
    underflows: Arc<AtomicU64>,
}

impl Playback {
    /// Open the output on `device` at its native rate/format and start playback.
    pub fn open(device: &cpal::Device, ring_capacity: usize) -> Result<Self> {
        if ring_capacity == 0 {
            return Err(AudioError::Config(
                "ring capacity must be non-zero".to_string(),
            ));
        }
        let config = device.default_output_config()?;
        let sample_format = config.sample_format();
        let channels = config.channels();
        let sample_rate = config.sample_rate().0;
        if channels == 0 {
            return Err(AudioError::Config(
                "output device reports zero channels".to_string(),
            ));
        }

        let (producer, mut consumer) = RingBuffer::new(ring_capacity);
        let underflows = Arc::new(AtomicU64::new(0));
        let underflows_cb = Arc::clone(&underflows);

        let stream = device.build_output_stream_raw(
            &config.config(),
            sample_format,
            move |data: &mut Data, _info: &OutputCallbackInfo| {
                fill_from_ring(data, &mut consumer, channels, &underflows_cb);
            },
            log_output_error,
            None,
        )?;
        stream.play()?;

        Ok(Self {
            producer,
            stream: Some(stream),
            sample_rate,
            channels,
            mixer: Mixer::default(),
            underflows,
        })
    }

    /// Build a playback half without hardware (tests, injected rings).
    ///
    /// The ring carries mono `f32` samples at `sample_rate`.
    pub fn from_parts(producer: Producer<f32>, sample_rate: u32, channels: u16) -> Self {
        Self {
            producer,
            stream: None,
            sample_rate,
            channels,
            mixer: Mixer::default(),
            underflows: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Push mono device-rate samples to the speaker ring (audio worker thread).
    /// Returns the number of samples accepted; the rest are dropped (ring full).
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let mut pushed = 0;
        for &sample in samples {
            if self.producer.push(sample).is_ok() {
                pushed += 1;
            }
        }
        pushed
    }

    /// Mutable access to the per-peer volume / deafen state.
    pub fn mixer_mut(&mut self) -> &mut Mixer {
        &mut self.mixer
    }

    /// The per-peer volume / deafen state.
    pub fn mixer(&self) -> &Mixer {
        &self.mixer
    }

    /// The device's native sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The device's channel count (mono input is duplicated per channel).
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Number of underflows (zero-filled output frames). Diagnostic only.
    pub fn underflow_count(&self) -> u64 {
        self.underflows.load(Ordering::Relaxed)
    }
}

/// RT-safe: pop mono from the ring (zero-fill on underflow), expand to the
/// device's channel layout, convert to the device's sample format.
fn fill_from_ring(
    data: &mut Data,
    consumer: &mut Consumer<f32>,
    channels: u16,
    underflows: &AtomicU64,
) {
    if channels == 0 {
        return;
    }
    match data.sample_format() {
        SampleFormat::F32 => fill_typed::<f32>(data, consumer, channels, underflows),
        SampleFormat::F64 => fill_typed::<f64>(data, consumer, channels, underflows),
        SampleFormat::I8 => fill_typed::<i8>(data, consumer, channels, underflows),
        SampleFormat::I16 => fill_typed::<i16>(data, consumer, channels, underflows),
        SampleFormat::I32 => fill_typed::<i32>(data, consumer, channels, underflows),
        SampleFormat::I64 => fill_typed::<i64>(data, consumer, channels, underflows),
        SampleFormat::U8 => fill_typed::<u8>(data, consumer, channels, underflows),
        SampleFormat::U16 => fill_typed::<u16>(data, consumer, channels, underflows),
        SampleFormat::U32 => fill_typed::<u32>(data, consumer, channels, underflows),
        SampleFormat::U64 => fill_typed::<u64>(data, consumer, channels, underflows),
        _ => {}
    }
}

fn fill_typed<T>(
    data: &mut Data,
    consumer: &mut Consumer<f32>,
    channels: u16,
    underflows: &AtomicU64,
) where
    T: SizedSample + FromSample<f32>,
{
    let Some(buffer) = data.as_slice_mut::<T>() else {
        return;
    };
    for frame in buffer.chunks_exact_mut(usize::from(channels)) {
        // Zero-fill on underflow: silence, never crackle or stale samples.
        let sample = consumer.pop().unwrap_or(0.0);
        for slot in frame {
            *slot = Sample::from_sample::<f32>(sample);
        }
    }
    if consumer.is_empty() {
        underflows.fetch_add(1, Ordering::Relaxed);
    }
}

fn log_output_error(err: StreamError) {
    tracing::error!(target: "zancord_audio", ?err, "output stream error");
}
