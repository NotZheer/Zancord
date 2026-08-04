//! Mic capture via cpal (Phase 1C.1). Callback must be RT-safe: only
//! `rtrb::Producer::push()` inside the callback; everything else on a worker.
//!
//! cpal 0.15 passes a dynamically typed `&Data` to raw stream callbacks (no
//! channel arguments); the callback converts whatever the device delivers to
//! `f32` and pushes it into the lock-free ring. Interleaved multi-channel
//! device audio is downmixed to mono later, in `resampler.rs`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Data, FromSample, InputCallbackInfo, Sample, SampleFormat, SizedSample, StreamError};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::error::{AudioError, Result};

/// Default mic ring capacity: 1 second of device-rate audio (interleaved).
pub const DEFAULT_RING_CAPACITY: usize = 48_000;

/// Microphone capture stream plus the worker-side ring consumer.
///
/// The consumer is drained by the audio worker thread (`pipeline.rs`); the
/// producer lives inside the cpal callback and is never touched elsewhere.
pub struct MicCapture {
    consumer: Consumer<f32>,
    // Kept alive for the stream's lifetime; dropping it stops capture.
    #[allow(dead_code)]
    stream: Option<cpal::Stream>,
    sample_rate: u32,
    channels: u16,
    overflowed: Arc<AtomicU64>,
}

impl MicCapture {
    /// Open the mic on `device` at its native rate/format and start capture.
    pub fn open(device: &cpal::Device, ring_capacity: usize) -> Result<Self> {
        if ring_capacity == 0 {
            return Err(AudioError::Config(
                "ring capacity must be non-zero".to_string(),
            ));
        }
        let config = device.default_input_config()?;
        let sample_format = config.sample_format();
        let channels = config.channels();
        let sample_rate = config.sample_rate().0;
        if channels == 0 {
            return Err(AudioError::Config(
                "input device reports zero channels".to_string(),
            ));
        }

        let (mut producer, consumer) = RingBuffer::new(ring_capacity);
        let overflowed = Arc::new(AtomicU64::new(0));
        let overflowed_cb = Arc::clone(&overflowed);

        let stream = device.build_input_stream_raw(
            &config.config(),
            sample_format,
            move |data: &Data, _info: &InputCallbackInfo| {
                copy_to_ring(data, &mut producer, &overflowed_cb);
            },
            log_input_error,
            None,
        )?;
        stream.play()?;

        Ok(Self {
            consumer,
            stream: Some(stream),
            sample_rate,
            channels,
            overflowed,
        })
    }

    /// Build a capture half without hardware (tests, injected rings).
    ///
    /// The ring carries mono `f32` samples at `sample_rate`.
    pub fn from_ring(consumer: Consumer<f32>, sample_rate: u32, channels: u16) -> Self {
        Self {
            consumer,
            stream: None,
            sample_rate,
            channels,
            overflowed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The worker-side end of the mic ring (interleaved `f32`, device rate).
    pub fn consumer(&mut self) -> &mut Consumer<f32> {
        &mut self.consumer
    }

    /// The device's native sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The device's channel count; ring samples are interleaved per frame.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Number of samples dropped because the ring was full (worker never
    /// drained fast enough). Diagnostic only.
    pub fn overflow_count(&self) -> u64 {
        self.overflowed.load(Ordering::Relaxed)
    }
}

/// RT-safe: convert the host's samples to `f32` and push them into the ring.
///
/// No allocation, no locking, no I/O. On overflow the sample is dropped and an
/// atomic counter is bumped — dropping is the only legal option in a callback.
fn copy_to_ring(data: &Data, producer: &mut Producer<f32>, overflowed: &AtomicU64) {
    match data.sample_format() {
        SampleFormat::F32 => push_typed::<f32>(data, producer, overflowed),
        SampleFormat::F64 => push_typed::<f64>(data, producer, overflowed),
        SampleFormat::I8 => push_typed::<i8>(data, producer, overflowed),
        SampleFormat::I16 => push_typed::<i16>(data, producer, overflowed),
        SampleFormat::I32 => push_typed::<i32>(data, producer, overflowed),
        SampleFormat::I64 => push_typed::<i64>(data, producer, overflowed),
        SampleFormat::U8 => push_typed::<u8>(data, producer, overflowed),
        SampleFormat::U16 => push_typed::<u16>(data, producer, overflowed),
        SampleFormat::U32 => push_typed::<u32>(data, producer, overflowed),
        SampleFormat::U64 => push_typed::<u64>(data, producer, overflowed),
        _ => {}
    }
}

fn push_typed<T>(data: &Data, producer: &mut Producer<f32>, overflowed: &AtomicU64)
where
    T: SizedSample + Copy,
    f32: FromSample<T>,
{
    if let Some(samples) = data.as_slice::<T>() {
        for &sample in samples {
            if producer
                .push(<T as Sample>::to_sample::<f32>(sample))
                .is_err()
            {
                overflowed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn log_input_error(err: StreamError) {
    tracing::error!(target: "zancord_audio", ?err, "input stream error");
}
