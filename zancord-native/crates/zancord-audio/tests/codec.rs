//! Opus codec tests (Phase 1C.8): encode→decode roundtrip within lossy
//! tolerance, PLC on missing packets, frame-size validation. Hardware-free.

use zancord_audio::codec::{OpusDecoder, OpusEncoder, FRAME_SIZE, MAX_PACKET_BYTES, SAMPLE_RATE};

fn sine_i16(freq: f32, n: usize, amplitude: f32) -> Vec<i16> {
    let phase = 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE as f32;
    (0..n)
        .map(|i| (amplitude * 32767.0 * (phase * i as f32).sin()) as i16)
        .collect()
}

fn rms(samples: &[i16]) -> f32 {
    let sum_sq: f32 = samples.iter().map(|&s| f32::from(s) * f32::from(s)).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[test]
fn opus_roundtrip_within_lossy_tolerance() {
    let mut encoder = OpusEncoder::new(32_000).unwrap();
    let mut decoder = OpusDecoder::new().unwrap();
    // Multiple frames: a single frame carries FEC overhead plus codec warm-up,
    // which unfairly inflates single-frame error. 10 frames = 200 ms.
    let input: Vec<i16> = sine_i16(440.0, FRAME_SIZE * 10, 0.5);
    let mut decoded = Vec::with_capacity(input.len());
    for chunk in input.chunks(FRAME_SIZE) {
        let packet = encoder.encode(chunk).unwrap();
        assert!(!packet.is_empty(), "encoder produced an empty packet");
        assert!(packet.len() <= MAX_PACKET_BYTES);
        let mut out = vec![0i16; FRAME_SIZE];
        let n = decoder.decode(Some(&packet), &mut out).unwrap();
        assert_eq!(n, FRAME_SIZE, "decoded frame must be 960 samples");
        decoded.extend_from_slice(&out[..n]);
    }

    // 32 kbps Opus with FEC is lossy: allow ~20% RMS error on a clean sine.
    let err = rms(&decoded) - rms(&input);
    let tolerance = 0.2 * rms(&input);
    assert!(
        err.abs() < tolerance,
        "roundtrip error {err} exceeds tolerance {tolerance}"
    );
}

#[test]
fn opus_plc_conceals_missing_packet() {
    let mut decoder = OpusDecoder::new().unwrap();
    let mut out = vec![0i16; FRAME_SIZE];
    let n = decoder.decode(None, &mut out).unwrap();
    assert_eq!(n, FRAME_SIZE, "PLC must produce a full frame");
    // Fresh decoder + PLC = near-silence (muted frame), not garbage.
    let energy: f32 = out.iter().map(|&s| f32::from(s) * f32::from(s)).sum();
    assert!(
        energy < FRAME_SIZE as f32 * 100.0 * 100.0,
        "PLC produced loud output: energy {energy}"
    );
}

#[test]
fn opus_plc_after_real_frames_is_bounded() {
    let mut encoder = OpusEncoder::new(32_000).unwrap();
    let mut decoder = OpusDecoder::new().unwrap();
    let input = sine_i16(440.0, FRAME_SIZE, 0.8);
    let packet = encoder.encode(&input).unwrap();

    // Prime the decoder with two real frames, then conceal two losses.
    let mut out = vec![0i16; FRAME_SIZE];
    decoder.decode(Some(&packet), &mut out).unwrap();
    decoder.decode(Some(&packet), &mut out).unwrap();
    let n = decoder.decode(None, &mut out).unwrap();
    assert_eq!(n, FRAME_SIZE);
    let n = decoder.decode(None, &mut out).unwrap();
    assert_eq!(n, FRAME_SIZE);

    // PLC after speech repeats pitch — energy must stay in the same ballpark
    // (well below clipping, above digital silence).
    let energy: f32 = out.iter().map(|&s| f32::from(s) * f32::from(s)).sum();
    let sig_energy = FRAME_SIZE as f32 * (0.8f32 * 32767.0f32).powi(2);
    assert!(
        energy > 0.0 && energy < 2.0 * sig_energy,
        "PLC energy {energy} out of bounds vs signal {sig_energy}"
    );
}

#[test]
fn encoder_rejects_wrong_frame_size() {
    let mut encoder = OpusEncoder::new(32_000).unwrap();
    assert!(
        encoder.encode(&[0i16; 480]).is_err(),
        "480-sample frame must be rejected"
    );
    assert!(encoder.encode(&[0i16; FRAME_SIZE]).is_ok());
}

#[test]
fn decoder_rejects_small_output_buffer() {
    let mut decoder = OpusDecoder::new().unwrap();
    assert!(decoder.decode(None, &mut [0i16; 100]).is_err());
}

#[test]
fn encoder_fec_and_bitrate_settable() {
    let mut encoder = OpusEncoder::new(32_000).unwrap();
    assert!(encoder.fec(), "in-band FEC must be on by default");
    encoder.set_fec(false).unwrap();
    assert!(!encoder.fec());
    encoder.set_bitrate(16_000).unwrap();
    assert_eq!(encoder.bitrate(), 16_000);
}
