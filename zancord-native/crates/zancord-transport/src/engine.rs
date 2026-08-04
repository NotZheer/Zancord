//! Media engine setup (Phase 1D.1): register Opus/VP8/H.264 codecs and build
//! the configured `webrtc::api::API` used by every peer connection.

use anyhow::Result;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS, MIME_TYPE_VP8};
use webrtc::api::{APIBuilder, API};
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecParameters, RTPCodecType};
use webrtc::rtp_transceiver::RTCPFeedback;

use crate::tracks::{H264_FMTP, OPUS_FMTP};

/// Opus payload type (48 kHz, stereo, FEC + DTX).
pub const OPUS_PAYLOAD_TYPE: u8 = 111;
/// VP8 payload type (90 kHz).
pub const VP8_PAYLOAD_TYPE: u8 = 96;
/// H.264 payload type (90 kHz, constrained baseline).
pub const H264_PAYLOAD_TYPE: u8 = 102;

/// Video RTCP feedback capabilities: REMB bitrate estimation, FIR/PLI
/// keyframe requests, and NACK.
fn video_rtcp_feedback() -> Vec<RTCPFeedback> {
    vec![
        RTCPFeedback {
            typ: "goog-remb".to_owned(),
            parameter: String::new(),
        },
        RTCPFeedback {
            typ: "ccm".to_owned(),
            parameter: "fir".to_owned(),
        },
        RTCPFeedback {
            typ: "nack".to_owned(),
            parameter: String::new(),
        },
        RTCPFeedback {
            typ: "nack".to_owned(),
            parameter: "pli".to_owned(),
        },
    ]
}

/// Builds a media engine with exactly the Zancord codec set:
/// Opus (PT 111, FEC + DTX), VP8 (PT 96, PLI/FIR/REMB/NACK),
/// H.264 (PT 102, constrained baseline, PLI/FIR/REMB/NACK).
///
/// The fmtp lines must match `TrackKind::codec()` in `tracks.rs`.
pub fn new_media_engine() -> Result<MediaEngine> {
    let mut engine = MediaEngine::default();

    engine.register_codec(
        RTCRtpCodecParameters {
            capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: OPUS_FMTP.to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: OPUS_PAYLOAD_TYPE,
            ..Default::default()
        },
        RTPCodecType::Audio,
    )?;

    engine.register_codec(
        RTCRtpCodecParameters {
            capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: MIME_TYPE_VP8.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: String::new(),
                rtcp_feedback: video_rtcp_feedback(),
            },
            payload_type: VP8_PAYLOAD_TYPE,
            ..Default::default()
        },
        RTPCodecType::Video,
    )?;

    engine.register_codec(
        RTCRtpCodecParameters {
            capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: H264_FMTP.to_owned(),
                rtcp_feedback: video_rtcp_feedback(),
            },
            payload_type: H264_PAYLOAD_TYPE,
            ..Default::default()
        },
        RTPCodecType::Video,
    )?;

    Ok(engine)
}

/// Builds the shared `API` (media engine + default no-op interceptor registry).
pub fn build_api() -> Result<API> {
    Ok(APIBuilder::new()
        .with_media_engine(new_media_engine()?)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn media_engine_offers_zancord_codecs() {
        let api = build_api().expect("api builds");
        let pc = api
            .new_peer_connection(Default::default())
            .await
            .expect("pc builds");
        // One audio + two video tracks so all registered codecs appear in SDP.
        for kind in crate::tracks::TrackKind::ALL {
            let track =
                webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample::new(
                    kind.codec(),
                    kind.id().to_owned(),
                    "s".to_owned(),
                );
            let _ = pc
                .add_track(std::sync::Arc::new(track))
                .await
                .expect("track added");
        }
        let offer = pc.create_offer(None).await.expect("offer created");
        let sdp = offer.sdp;

        assert!(sdp.contains("a=rtpmap:111 opus/48000/2"), "opus in offer");
        assert!(sdp.contains("useinbandfec=1"), "opus fec in offer");
        assert!(sdp.contains("usedtx=1"), "opus dtx in offer");
        assert!(sdp.contains("a=rtpmap:96 VP8/90000"), "vp8 in offer");
        assert!(sdp.contains("a=rtpmap:102 H264/90000"), "h264 in offer");
        assert!(
            sdp.contains("profile-level-id=42e01f"),
            "h264 profile in offer"
        );
        pc.close().await.expect("pc closes");
    }

    #[test]
    fn api_builds() {
        let api = build_api().expect("api builds");
        let _ = api.media_engine();
    }
}
