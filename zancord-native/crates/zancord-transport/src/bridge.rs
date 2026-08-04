//! RTP media bridge (Phase 1D.6): send loops write encoded frames from the
//! audio/video pipelines into local tracks; receive loops read RTP from
//! remote tracks and forward payloads into decode channels.

use std::time::Duration;

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc;
use tracing::{debug, warn};
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_remote::TrackRemote;

use zancord_protocol::{EncodedAudioFrame, EncodedVideoFrame};

/// Opus frame duration (48 kHz, 20 ms — matches the audio pipeline).
pub const AUDIO_FRAME_DURATION: Duration = Duration::from_millis(20);
/// Opus RTP clock rate.
const OPUS_CLOCK_RATE: u64 = 48_000;
/// Video RTP clock rate (VP8/H.264).
const VIDEO_CLOCK_RATE: u64 = 90_000;

/// Send loop for the shared mic/screen-audio track. Consumes encoded Opus
/// frames from the audio pipeline and writes one 20 ms `Sample` per frame.
///
/// Exits only when the channel closes. Write errors are non-fatal: the
/// answering side's track binds BEFORE ICE/DTLS completes, so early frames
/// legitimately fail until the transport is up — killing the loop there would
/// silence us forever.
pub async fn audio_send_loop(
    track: Arc<TrackLocalStaticSample>,
    mut rx: mpsc::Receiver<EncodedAudioFrame>,
) {
    let mut last_warn = std::time::Instant::now() - Duration::from_secs(6);
    while let Some(frame) = rx.recv().await {
        let sample = Sample {
            data: Bytes::from(frame.data),
            duration: AUDIO_FRAME_DURATION,
            ..Default::default()
        };
        if let Err(err) = track.write_sample(&sample).await {
            if last_warn.elapsed() >= Duration::from_secs(5) {
                warn!(error = %err, "audio write failed (transport not ready?); continuing");
                last_warn = std::time::Instant::now();
            }
        }
    }
    debug!("audio send loop ended (channel closed)");
}

/// Send loop for a shared video track (camera or screen). Writes one
/// `Sample` per encoded frame with `1000/fps` ms duration.
///
/// Exits only when the channel closes; write errors are non-fatal (see
/// [`audio_send_loop`] for why).
pub async fn video_send_loop(
    track: Arc<TrackLocalStaticSample>,
    mut rx: mpsc::Receiver<EncodedVideoFrame>,
) {
    let mut last_warn = std::time::Instant::now() - Duration::from_secs(6);
    while let Some(frame) = rx.recv().await {
        // Guard against a broken fps value (0 or absurd) — fall back to 33 ms.
        let millis = 1000u32.checked_div(frame.fps).unwrap_or(33).max(1);
        let sample = Sample {
            data: Bytes::from(frame.data),
            duration: Duration::from_millis(millis as u64),
            ..Default::default()
        };
        if let Err(err) = track.write_sample(&sample).await {
            if last_warn.elapsed() >= Duration::from_secs(5) {
                warn!(error = %err, "video write failed (transport not ready?); continuing");
                last_warn = std::time::Instant::now();
            }
        }
    }
    debug!("video send loop ended (channel closed)");
}

/// Receive loop for a remote audio track. Forwards every RTP payload as an
/// `EncodedAudioFrame` (an Opus packet) into `tx`.
pub async fn audio_receive_loop(track: Arc<TrackRemote>, tx: mpsc::Sender<EncodedAudioFrame>) {
    loop {
        match track.read_rtp().await {
            Ok((pkt, _)) => {
                let frame = EncodedAudioFrame {
                    data: pkt.payload.to_vec(),
                    sequence: pkt.header.sequence_number as u64,
                    timestamp_ms: rtp_timestamp_ms(pkt.header.timestamp, OPUS_CLOCK_RATE),
                };
                if tx.send(frame).await.is_err() {
                    return; // decoder channel closed
                }
            }
            Err(err) => {
                debug!(error = %err, "audio receive loop ending");
                return;
            }
        }
    }
}

/// Receive loop for a remote video track. Forwards every RTP payload as an
/// `EncodedVideoFrame` with a best-effort keyframe flag.
pub async fn video_receive_loop(track: Arc<TrackRemote>, tx: mpsc::Sender<EncodedVideoFrame>) {
    loop {
        match track.read_rtp().await {
            Ok((pkt, _)) => {
                let mime = track.codec().capability.mime_type.to_lowercase();
                let keyframe = if mime.contains("vp8") {
                    vp8_is_keyframe(&pkt.payload)
                } else if mime.contains("h264") {
                    h264_is_keyframe(&pkt.payload)
                } else {
                    false
                };
                let frame = EncodedVideoFrame {
                    data: pkt.payload.to_vec(),
                    keyframe,
                    width: 0,
                    height: 0,
                    fps: 0,
                    timestamp_ms: rtp_timestamp_ms(pkt.header.timestamp, VIDEO_CLOCK_RATE),
                };
                if tx.send(frame).await.is_err() {
                    return; // decoder channel closed
                }
            }
            Err(err) => {
                debug!(error = %err, "video receive loop ending");
                return;
            }
        }
    }
}

/// Converts an RTP timestamp (clock-rate units) to milliseconds.
fn rtp_timestamp_ms(rtp_ts: u32, clock_rate: u64) -> u64 {
    (rtp_ts as u64).saturating_mul(1000) / clock_rate.max(1)
}

/// Best-effort VP8 keyframe detection from the RTP payload descriptor.
///
/// Layout: octet 0 (X/S bits), optional extension octet + fields, then the
/// VP8 frame header when S (start of partition) is set; the frame header's
/// P bit (0 = keyframe) is the lowest bit of the first frame-header octet.
fn vp8_is_keyframe(payload: &[u8]) -> bool {
    let Some(&first) = payload.first() else {
        return false;
    };
    if first & 0x10 == 0 {
        return false; // not the start of a frame partition
    }
    let x = first & 0x80 != 0;
    let mut idx = 1usize;
    if x {
        let Some(&ext) = payload.get(idx) else {
            return false;
        };
        idx += 1;
        if ext & 0x80 != 0 {
            // PictureID: 1 byte, or 2 when the high bit of the first byte is set.
            idx += match payload.get(idx) {
                Some(pid) if pid & 0x80 != 0 => 2,
                Some(_) => 1,
                None => return false,
            };
        }
        if ext & 0x40 != 0 {
            idx += 1; // tl0picidx
        }
        if ext & 0x20 != 0 || ext & 0x10 != 0 {
            idx += 1; // tids / keyidx
        }
    }
    match payload.get(idx) {
        Some(frame_header) => frame_header & 0x01 == 0,
        None => false,
    }
}

/// Best-effort H.264 keyframe detection: NAL unit type 5 (IDR) or 7 (SPS);
/// also scans STAP-A aggregates (type 24) for contained IDR/SPS NALs.
fn h264_is_keyframe(payload: &[u8]) -> bool {
    let Some(&first) = payload.first() else {
        return false;
    };
    let nal_type = first & 0x1f;
    match nal_type {
        5 | 7 => true,
        24 => stap_a_contains_keyframe(&payload[1..]),
        _ => false,
    }
}

fn stap_a_contains_keyframe(rest: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 2 <= rest.len() {
        let nal_size = u16::from_be_bytes([rest[i], rest[i + 1]]) as usize;
        i += 2;
        if nal_size == 0 || i + nal_size > rest.len() {
            return false;
        }
        if matches!(rest[i] & 0x1f, 5 | 7) {
            return true;
        }
        i += nal_size;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_timestamp_conversion() {
        // 48000 Hz, 1 second => 1000 ms.
        assert_eq!(rtp_timestamp_ms(48_000, 48_000), 1000);
        // 90000 Hz, half a second => 500 ms.
        assert_eq!(rtp_timestamp_ms(45_000, 90_000), 500);
        assert_eq!(rtp_timestamp_ms(0, 90_000), 0);
    }

    #[test]
    fn vp8_keyframe_detection() {
        // Simple descriptor: X=0, S=1, then frame header P=0 (keyframe).
        let keyframe = [0x10, 0x00];
        assert!(vp8_is_keyframe(&keyframe));
        // P=1 (inter frame).
        let inter = [0x10, 0x01];
        assert!(!vp8_is_keyframe(&inter));
        // Continuation packet (S=0): never a keyframe start.
        let continuation = [0x00, 0x00];
        assert!(!vp8_is_keyframe(&continuation));
        // With extension octet + picture id (1-byte pid 0x2B).
        let with_ext = [0x90, 0x80, 0x2b, 0x00]; // X=1,S=1; I=1; pid=0x2B; P=0
        assert!(vp8_is_keyframe(&with_ext));
        let with_ext_inter = [0x90, 0x80, 0x2b, 0x01];
        assert!(!vp8_is_keyframe(&with_ext_inter));
        assert!(!vp8_is_keyframe(&[]));
    }

    #[test]
    fn h264_keyframe_detection() {
        assert!(h264_is_keyframe(&[0x65, 0x88, 0x84])); // IDR
        assert!(h264_is_keyframe(&[0x67, 0x42, 0xe0])); // SPS
        assert!(!h264_is_keyframe(&[0x41, 0x9a])); // non-IDR slice
                                                   // STAP-A containing an SPS NAL.
        let stap = [0x78, 0x00, 0x03, 0x67, 0x42, 0xe0];
        assert!(h264_is_keyframe(&stap));
        let stap_inter = [0x78, 0x00, 0x02, 0x41, 0x9a];
        assert!(!h264_is_keyframe(&stap_inter));
        assert!(!h264_is_keyframe(&[]));
    }

    #[test]
    fn video_frame_duration_fallback() {
        let millis = |fps: u32| -> u32 { 1000u32.checked_div(fps).unwrap_or(33).max(1) };
        assert_eq!(millis(30), 33);
        assert_eq!(millis(0), 33);
        assert_eq!(millis(60), 16);
    }
}
