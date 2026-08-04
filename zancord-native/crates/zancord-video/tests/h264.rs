//! H.264 codec tests (Phase 3C.7): encode/decode roundtrip within lossy
//! tolerance, forced keyframes, resolution switching. Hardware-free.

use zancord_protocol::VideoCodec;
use zancord_video::codec::{create_decoder, create_encoder, VideoEncoderConfig};
use zancord_video::convert::I420Frame;

fn synthetic_frame(width: u32, height: u32, seed: u8) -> I420Frame {
    let (w, h) = (width as usize, height as usize);
    let mut y = vec![0u8; w * h];
    let mut u = vec![128u8; w * h / 4];
    let mut v = vec![128u8; w * h / 4];
    // Moving gradient + salt so frames differ and the encoder has work to do.
    for row in 0..h {
        for col in 0..w {
            y[row * w + col] = 16u8
                .wrapping_add((row * 255 / h) as u8)
                .wrapping_add(seed * 7)
                .clamp(16, 235);
        }
    }
    for (i, val) in y.iter_mut().enumerate() {
        if i % 97 == 0 {
            *val = val.wrapping_add(seed);
        }
    }
    u[0] = 90;
    v[0] = 240;
    I420Frame {
        width,
        height,
        y,
        u,
        v,
    }
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i16 - *y as i16).unsigned_abs() as f64)
        .sum::<f64>()
        / a.len() as f64
}

#[test]
fn h264_roundtrip_within_lossy_tolerance() {
    let config = VideoEncoderConfig {
        codec: VideoCodec::H264,
        width: 320,
        height: 240,
        fps: 30,
        bitrate_bps: 600_000,
    };
    let mut encoder = create_encoder(&config).expect("encoder");
    let mut decoder = create_decoder(VideoCodec::H264).expect("decoder");

    let mut decoded: Option<I420Frame> = None;
    for i in 0..15u8 {
        let frame = synthetic_frame(320, 240, i);
        let encoded = encoder.encode(&frame).expect("encode");
        assert!(!encoded.data.is_empty(), "empty bitstream");
        if i == 0 {
            assert!(encoded.keyframe, "first frame must be a keyframe");
        }
        if let Some(out) = decoder.decode(&encoded.data).expect("decode") {
            decoded = Some(out);
        }
    }

    let decoded = decoded.expect("decoder produced no frame");
    assert_eq!((decoded.width, decoded.height), (320, 240));

    // Lossy codec: mean abs diff on Y should be well below signal variance.
    let original = synthetic_frame(320, 240, 14);
    let diff = mean_abs_diff(&decoded.y, &original.y);
    assert!(
        diff < 12.0,
        "Y mean abs diff too large after H.264 roundtrip: {diff}"
    );
}

#[test]
fn h264_forced_keyframe_produces_idr() {
    let mut config = VideoEncoderConfig::new(VideoCodec::H264);
    config.width = 160;
    config.height = 120;
    let mut encoder = create_encoder(&config).expect("encoder");

    // Warm up with a few delta frames.
    for i in 0..10u8 {
        let encoded = encoder
            .encode(&synthetic_frame(160, 120, i))
            .expect("encode");
        if i > 0 {
            assert!(!encoded.keyframe, "delta frames must not be keyframes");
        }
    }

    encoder.force_keyframe();
    let encoded = encoder
        .encode(&synthetic_frame(160, 120, 20))
        .expect("encode");
    assert!(encoded.keyframe, "forced keyframe must be an IDR");
}

#[test]
fn h264_resolution_switch_reinitializes() {
    let mut encoder = create_encoder(&VideoEncoderConfig::new(VideoCodec::H264)).expect("encoder");
    let mut decoder = create_decoder(VideoCodec::H264).expect("decoder");

    encoder
        .encode(&synthetic_frame(320, 240, 1))
        .expect("encode at 320x240");
    let encoded = encoder
        .encode(&synthetic_frame(160, 120, 2))
        .expect("encode at 160x120");
    assert!(
        encoded.keyframe,
        "resolution change must produce a keyframe"
    );

    let out = decoder
        .decode(&encoded.data)
        .expect("decode")
        .expect("frame");
    assert_eq!((out.width, out.height), (160, 120));
}

#[test]
fn h264_decoder_survives_garbage() {
    let mut decoder = create_decoder(VideoCodec::H264).expect("decoder");
    // Corrupt/missing data must not panic; it yields None or an error.
    let _ = decoder.decode(b"this is not h264 data at all........");
    let _ = decoder.decode(&[]);
}
