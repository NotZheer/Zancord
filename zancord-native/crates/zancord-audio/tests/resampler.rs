//! Resampler tests (Phase 1C.8): sample-count math for 44100↔48000, roundtrip
//! drift tolerance, stereo→mono downmix. Hardware-free.

use zancord_audio::resampler::{
    downmix_to_mono_into, CaptureResampler, PlaybackResampler, FRAME_SIZE,
};

fn sine(freq: f32, sample_rate: u32, n: usize, amplitude: f32) -> Vec<f32> {
    let phase = 2.0 * std::f32::consts::PI * freq / sample_rate as f32;
    (0..n)
        .map(|i| amplitude * (phase * i as f32).sin())
        .collect()
}

#[test]
fn capture_24000_incremental_cadence() {
    // The production pipeline feeds the capture resampler in device-callback
    // sized chunks (480 samples = 20 ms at 24 kHz), one per tick. 48,000 input
    // samples = 1 s at 24 kHz = 2 s at 48 kHz = 100 frames of 960 samples
    // (rubato's filter latency costs the first few samples).
    let mut resampler = CaptureResampler::new(24_000, 1).unwrap();
    let mut frames = 0usize;
    let mut out = vec![0.0; FRAME_SIZE];
    for _ in 0..100 {
        resampler.push(&[0.5; 480]).unwrap();
        while resampler.take_frame(&mut out) {
            frames += 1;
        }
    }
    assert!(
        (95..=100).contains(&frames),
        "expected ~100 frames from 1s at 24kHz, got {frames}"
    );
}

#[test]
fn capture_44100_to_48000_sample_count() {
    let mut resampler = CaptureResampler::new(44_100, 1).unwrap();
    let input = sine(440.0, 44_100, 44_100 * 2, 0.5); // 2 s mono
    resampler.push(&input).unwrap();
    // 2 s in ≈ 2 s out; fixed-input chunking truncates the tail (< 2% error).
    let out = resampler.pending_len() as f64;
    let expected = 96_000.0;
    assert!(
        (out - expected).abs() / expected < 0.02,
        "capture resampler drift: {out} vs {expected}"
    );
    // The partial chunk stays buffered as device-rate input.
    assert_eq!(resampler.input_buffered(), 44_100 * 2 % FRAME_SIZE);
}

#[test]
fn playback_48000_to_44100_sample_count() {
    let mut resampler = PlaybackResampler::new(44_100).unwrap();
    let mut total = 0usize;
    for _ in 0..10 {
        total += resampler.process(&[0.0; FRAME_SIZE]).unwrap().len();
    }
    let after_10 = total;
    for _ in 0..10 {
        total += resampler.process(&[0.0; FRAME_SIZE]).unwrap().len();
    }
    // Steady-state: 10 chunks × 960 samples @48k → 10 × 882 @44.1k (± jitter).
    let steady = total - after_10;
    let expected = 10 * FRAME_SIZE * 44_100 / 48_000; // 8820
    assert!(
        (steady as i64 - expected as i64).abs() <= 10,
        "playback resampler steady-state {steady} vs {expected}"
    );
}

#[test]
fn roundtrip_44100_via_48000_within_tolerance() {
    let mut up = CaptureResampler::new(44_100, 1).unwrap();
    let mut down = PlaybackResampler::new(44_100).unwrap();
    let input = sine(440.0, 44_100, 44_100 * 4, 0.5); // 4 s
    up.push(&input).unwrap();

    let mut total_down = 0usize;
    let mut frame = [0.0f32; FRAME_SIZE];
    while up.take_frame(&mut frame) {
        let out = down.process(&frame).unwrap();
        total_down += out.len();
    }
    // Fixed-input async resampling drifts a couple of percent per hop; the
    // ring buffers absorb that drift in the real pipeline.
    let expected = 44_100 * 4;
    assert!(
        (total_down as f64 - expected as f64).abs() / (expected as f64) < 0.03,
        "roundtrip drift: {total_down} vs {expected}"
    );
}

#[test]
fn stereo_downmix_to_mono() {
    let mut out = Vec::new();
    downmix_to_mono_into(&[0.4, 0.2, 0.4, 0.2, 0.0, 0.0], 2, &mut out);
    assert_eq!(out, vec![0.3, 0.3, 0.0]);
}

#[test]
fn stereo_capture_resamples_to_mono() {
    let mut resampler = CaptureResampler::new(48_000, 2).unwrap();
    // 3 chunks of interleaved (L=0.4, R=0.2) → mono 0.3.
    let mut interleaved = Vec::with_capacity(FRAME_SIZE * 2 * 3);
    for _ in 0..(FRAME_SIZE * 3) {
        interleaved.push(0.4);
        interleaved.push(0.2);
    }
    resampler.push(&interleaved).unwrap();

    let mut frames = Vec::new();
    let mut frame = [0.0f32; FRAME_SIZE];
    while resampler.take_frame(&mut frame) {
        frames.push(frame);
    }
    // First chunk is resampler warm-up; later frames must be steady 0.3.
    assert!(
        frames.len() >= 2,
        "expected ≥2 frames, got {}",
        frames.len()
    );
    for &sample in &frames[1] {
        assert!(
            (sample - 0.3).abs() < 0.01,
            "downmix value {sample} deviates from 0.3"
        );
    }
}

#[test]
fn mono_passthrough_keeps_samples() {
    let mut out = Vec::new();
    downmix_to_mono_into(&[0.1, -0.2, 0.3], 1, &mut out);
    assert_eq!(out, vec![0.1, -0.2, 0.3]);
}

#[test]
fn invalid_configs_rejected() {
    assert!(CaptureResampler::new(0, 1).is_err());
    assert!(CaptureResampler::new(48_000, 0).is_err());
    assert!(PlaybackResampler::new(0).is_err());
    let mut resampler = PlaybackResampler::new(48_000).unwrap();
    assert!(
        resampler.process(&[0.0; 100]).is_err(),
        "partial frame must be rejected"
    );
}
