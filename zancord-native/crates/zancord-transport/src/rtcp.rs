//! RTCP feedback handling (Phase 1D.7): incoming PLI/FIR from a remote peer
//! becomes a keyframe request signal for the local encoder (rate-limited to
//! one per 500 ms per peer/track); incoming REMB becomes a bitrate hint.

use std::time::{Duration, Instant};

use tracing::{debug, warn};
use webrtc::rtcp;

use crate::tracks::TrackKind;

/// Minimum interval between forwarded keyframe requests per (peer, track).
pub const KEYFRAME_RATE_LIMIT: Duration = Duration::from_millis(500);

/// Transport feedback signals consumed by the media pipeline (Phase 2+).
#[derive(Debug, Clone, PartialEq)]
pub enum RtcpFeedback {
    /// The remote decoder lost the stream; the local encoder for `track`
    /// should emit a keyframe immediately.
    KeyframeRequest { peer_id: String, track: TrackKind },
    /// Receiver-estimated maximum bitrate for `track`; the local pipeline
    /// should not exceed this aggregate bitrate for the peer.
    BitrateHint {
        peer_id: String,
        track: TrackKind,
        bitrate_bps: u32,
    },
}

/// Rate-limits keyframe request forwarding (1 per 500 ms).
#[derive(Debug)]
pub struct KeyframeRateLimiter {
    interval: Duration,
    last_allowed: Option<Instant>,
}

impl Default for KeyframeRateLimiter {
    fn default() -> Self {
        Self::new(KEYFRAME_RATE_LIMIT)
    }
}

impl KeyframeRateLimiter {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_allowed: None,
        }
    }

    /// Returns `true` if a keyframe request may be forwarded now.
    pub fn allowed(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last_allowed
            .is_some_and(|last| now.duration_since(last) < self.interval)
        {
            return false;
        }
        self.last_allowed = Some(now);
        true
    }
}

/// Classifies a batch of incoming RTCP packets (read from an `RTCRtpSender`)
/// into transport feedback signals for the local pipeline.
pub fn classify(
    packets: &[Box<dyn rtcp::packet::Packet + Send + Sync>],
    peer_id: &str,
    track: TrackKind,
    limiter: &mut KeyframeRateLimiter,
) -> Vec<RtcpFeedback> {
    let mut out = Vec::new();
    for pkt in packets {
        if pkt
            .as_any()
            .is::<rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication>()
            || pkt
                .as_any()
                .is::<rtcp::payload_feedbacks::full_intra_request::FullIntraRequest>()
        {
            if limiter.allowed() {
                debug!(peer = %peer_id, ?track, "keyframe requested via RTCP");
                out.push(RtcpFeedback::KeyframeRequest {
                    peer_id: peer_id.to_owned(),
                    track,
                });
            }
        } else if let Some(remb) = pkt
            .as_any()
            .downcast_ref::<rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate>()
        {
            if remb.bitrate.is_finite() && remb.bitrate > 0.0 {
                out.push(RtcpFeedback::BitrateHint {
                    peer_id: peer_id.to_owned(),
                    track,
                    bitrate_bps: remb.bitrate as u32,
                });
            }
        }
    }
    if out.is_empty() {
        return out;
    }
    out
}

/// Spawns the RTCP receive loop for one local video sender. RTCP from the
/// remote peer (PLI/FIR/REMB) arrives on the sender's RTCP stream and is
/// forwarded to `feedback_tx` after classification.
pub fn spawn_rtcp_receive_loop(
    sender: std::sync::Arc<webrtc::rtp_transceiver::rtp_sender::RTCRtpSender>,
    peer_id: String,
    track: TrackKind,
    feedback_tx: tokio::sync::broadcast::Sender<RtcpFeedback>,
) {
    tokio::spawn(async move {
        let mut limiter = KeyframeRateLimiter::default();
        loop {
            match sender.read_rtcp().await {
                Ok((packets, _)) => {
                    for fb in classify(&packets, &peer_id, track, &mut limiter) {
                        if feedback_tx.send(fb).is_err() {
                            // No receivers left (app gone); stop the loop.
                            return;
                        }
                    }
                }
                Err(err) => {
                    if matches!(
                        err,
                        webrtc::Error::ErrClosedPipe | webrtc::Error::ErrConnectionClosed
                    ) {
                        return;
                    }
                    warn!(peer = %peer_id, error = %err, "rtcp read failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtcp::payload_feedbacks::full_intra_request::{FirEntry, FullIntraRequest};
    use rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
    use rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate;

    fn boxed(
        pkt: impl rtcp::packet::Packet + Send + Sync + 'static,
    ) -> Box<dyn rtcp::packet::Packet + Send + Sync> {
        Box::new(pkt)
    }

    #[test]
    fn pli_produces_rate_limited_keyframe_request() {
        let mut limiter = KeyframeRateLimiter::new(Duration::from_millis(500));
        let packets = vec![boxed(PictureLossIndication {
            sender_ssrc: 1,
            media_ssrc: 2,
        })];

        let first = classify(&packets, "peer-a", TrackKind::Camera, &mut limiter);
        assert_eq!(
            first,
            vec![RtcpFeedback::KeyframeRequest {
                peer_id: "peer-a".to_owned(),
                track: TrackKind::Camera,
            }]
        );

        // Immediate repeat is suppressed by the rate limiter.
        let second = classify(&packets, "peer-a", TrackKind::Camera, &mut limiter);
        assert!(second.is_empty());
    }

    #[test]
    fn fir_is_treated_like_pli() {
        let mut limiter = KeyframeRateLimiter::new(Duration::from_millis(500));
        let packets = vec![boxed(FullIntraRequest {
            sender_ssrc: 1,
            media_ssrc: 2,
            fir: vec![FirEntry::default()],
        })];
        let out = classify(&packets, "peer-b", TrackKind::Screen, &mut limiter);
        assert_eq!(
            out,
            vec![RtcpFeedback::KeyframeRequest {
                peer_id: "peer-b".to_owned(),
                track: TrackKind::Screen,
            }]
        );
    }

    #[test]
    fn remb_produces_bitrate_hint() {
        let mut limiter = KeyframeRateLimiter::default();
        let packets = vec![boxed(ReceiverEstimatedMaximumBitrate {
            sender_ssrc: 1,
            bitrate: 1_500_000.0,
            ssrcs: vec![2],
        })];
        let out = classify(&packets, "peer-c", TrackKind::Camera, &mut limiter);
        assert_eq!(
            out,
            vec![RtcpFeedback::BitrateHint {
                peer_id: "peer-c".to_owned(),
                track: TrackKind::Camera,
                bitrate_bps: 1_500_000,
            }]
        );
    }

    #[test]
    fn rate_limiter_allows_after_interval() {
        let mut limiter = KeyframeRateLimiter::new(Duration::from_millis(1));
        assert!(limiter.allowed());
        assert!(!limiter.allowed());
        std::thread::sleep(Duration::from_millis(5));
        assert!(limiter.allowed());
    }
}
