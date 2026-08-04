//! Processor tests (Phase 1C.8): HPF attenuation/pass-through, noise gate
//! open/close, level metering. All hardware-free.

use zancord_audio::processor::{
    HighPassFilter, LevelMeter, NoiseGate, MAX_THRESHOLD_DB, MIN_THRESHOLD_DB,
};
use zancord_audio::resampler::{FRAME_SIZE, SAMPLE_RATE};

fn sine(freq: f32, sample_rate: u32, n: usize, amplitude: f32) -> Vec<f32> {
    let phase = 2.0 * std::f32::consts::PI * freq / sample_rate as f32;
    (0..n)
        .map(|i| amplitude * (phase * i as f32).sin())
        .collect()
}

fn rms(samples: &[f32]) -> f32 {
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[test]
fn hpf_heavily_attenuates_40hz() {
    let mut hpf = HighPassFilter::new(80.0, SAMPLE_RATE);
    let mut input = sine(40.0, SAMPLE_RATE, 4 * SAMPLE_RATE as usize, 1.0);
    let input_tail = input[(2 * SAMPLE_RATE as usize)..].to_vec();
    hpf.process_block(&mut input);
    let out_tail = &input[(2 * SAMPLE_RATE as usize)..];
    // 2nd-order Butterworth HPF at fc=80 Hz gives ~-12 dB at 40 Hz (|H|~0.24).
    assert!(
        rms(out_tail) < 0.35 * rms(&input_tail),
        "40 Hz leaked through: out {} vs in {}",
        rms(out_tail),
        rms(&input_tail)
    );
}

#[test]
fn hpf_passes_1khz_through() {
    let mut hpf = HighPassFilter::new(80.0, SAMPLE_RATE);
    let mut input = sine(1_000.0, SAMPLE_RATE, 2 * SAMPLE_RATE as usize, 0.5);
    hpf.process_block(&mut input);
    let out_tail = &input[SAMPLE_RATE as usize..];
    let expected = 0.5 / std::f32::consts::SQRT_2; // rms of a 0.5 sine
    assert!(
        (rms(out_tail) - expected).abs() < 0.05 * expected,
        "1 kHz attenuated: out {} vs expected {}",
        rms(out_tail),
        expected
    );
}

#[test]
fn gate_closes_on_silence() {
    let mut gate = NoiseGate::new(-45.0, SAMPLE_RATE);
    assert!((gate.gain() - 1.0).abs() < 1e-6, "gate starts open");
    let mut silence = vec![0.0; FRAME_SIZE];
    for _ in 0..50 {
        gate.process_frame(&mut silence);
    }
    // Release is 50 ms; 50 frames = 1 s is far beyond that.
    assert!(gate.gain() < 0.01, "gate stayed open: gain {}", gate.gain());
}

#[test]
fn gate_opens_on_loud_signal() {
    let mut gate = NoiseGate::new(-45.0, SAMPLE_RATE);
    // Close it first.
    let mut silence = vec![0.0; FRAME_SIZE];
    for _ in 0..10 {
        gate.process_frame(&mut silence);
    }
    assert!(gate.gain() < 0.01, "gate failed to close: {}", gate.gain());

    // Loud signal well above -45 dBFS (rms of 0.8 sine ≈ -4.9 dBFS).
    let mut loud = sine(1_000.0, SAMPLE_RATE, FRAME_SIZE, 0.8);
    for _ in 0..5 {
        gate.process_frame(&mut loud);
    }
    // Attack is 1 ms → fully open within one 20 ms frame.
    assert!(
        (gate.gain() - 1.0).abs() < 1e-3,
        "gate failed to open: gain {}",
        gate.gain()
    );
}

#[test]
fn gate_threshold_is_clamped() {
    let gate = NoiseGate::new(-100.0, SAMPLE_RATE);
    assert_eq!(gate.threshold_db(), MIN_THRESHOLD_DB);
    let gate = NoiseGate::new(0.0, SAMPLE_RATE);
    assert_eq!(gate.threshold_db(), MAX_THRESHOLD_DB);
}

#[test]
fn gate_silence_output_is_zero() {
    let mut gate = NoiseGate::new(-45.0, SAMPLE_RATE);
    let mut silence = vec![0.0; FRAME_SIZE];
    for _ in 0..10 {
        gate.process_frame(&mut silence);
    }
    assert!(silence.iter().all(|&s| s == 0.0));
}

#[test]
fn meter_emits_throttled_readings() {
    let mut meter = LevelMeter::new(SAMPLE_RATE, 50);
    let signal = sine(1_000.0, SAMPLE_RATE, FRAME_SIZE, 0.5);
    let mut readings = 0;
    let mut last = None;
    // 100 frames = 2 s. 50 ms at 48 kHz = 2400 samples = 2.5 frames, so the
    // sample-count throttle emits every 3rd frame: floor(100 / 3) = 33.
    for _ in 0..100 {
        if let Some(reading) = meter.process_frame(&signal) {
            readings += 1;
            last = Some(reading);
        }
    }
    assert_eq!(
        readings, 33,
        "expected 33 throttled readings (every 3rd frame)"
    );
    let reading = last.expect("meter never emitted");
    assert!((reading.peak - 0.5).abs() < 0.01, "peak {}", reading.peak);
    // 0.5-amplitude sine → RMS 0.5/√2 ≈ -9.03 dBFS → (60 - 9.03)/60.
    let rms_db = 20.0 * (0.5 / std::f32::consts::SQRT_2).log10();
    let expected = (rms_db + 60.0) / 60.0;
    assert!(
        (reading.rms - expected).abs() < 0.02,
        "rms {} vs expected {}",
        reading.rms,
        expected
    );
}

#[test]
fn meter_silence_reads_zero() {
    let mut meter = LevelMeter::new(SAMPLE_RATE, 50);
    let mut readings = 0;
    for _ in 0..100 {
        if meter.process_frame(&[0.0; FRAME_SIZE]).is_some() {
            readings += 1;
        }
    }
    assert_eq!(readings, 33);
}
