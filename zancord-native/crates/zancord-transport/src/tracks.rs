//! Track management (Phase 1D.5): shared local track instances (mic, camera,
//! screen, screen-audio) plus track-kind metadata used for routing and RTCP.
//!
//! Tracks are created once per process and SHARED across every peer
//! connection via `Arc`. `TrackLocalStaticSample::write_sample` fans a sample
//! out to every bound peer connection, so one send loop per track serves the
//! whole mesh.

use std::sync::Arc;

use webrtc::api::media_engine::{MIME_TYPE_H264, MIME_TYPE_OPUS};
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

/// msid stream id for all outgoing audio tracks.
pub const AUDIO_STREAM_ID: &str = "zancord-audio";
/// msid stream id for all outgoing video tracks.
pub const VIDEO_STREAM_ID: &str = "zancord-video";

/// Track ids (used as RTP msid track ids and for remote track routing).
pub const MIC_TRACK_ID: &str = "mic";
pub const CAMERA_TRACK_ID: &str = "camera";
pub const SCREEN_TRACK_ID: &str = "screen";
pub const SCREEN_AUDIO_TRACK_ID: &str = "screen-audio";

/// Identifies one logical outgoing track kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackKind {
    Mic,
    Camera,
    Screen,
    ScreenAudio,
}

impl TrackKind {
    /// All track kinds, mic first (stable iteration order).
    pub const ALL: [TrackKind; 4] = [
        TrackKind::Mic,
        TrackKind::Camera,
        TrackKind::Screen,
        TrackKind::ScreenAudio,
    ];

    /// The msid track id used on the wire.
    pub fn id(self) -> &'static str {
        match self {
            TrackKind::Mic => MIC_TRACK_ID,
            TrackKind::Camera => CAMERA_TRACK_ID,
            TrackKind::Screen => SCREEN_TRACK_ID,
            TrackKind::ScreenAudio => SCREEN_AUDIO_TRACK_ID,
        }
    }

    /// The msid stream id used on the wire.
    pub fn stream_id(self) -> &'static str {
        match self {
            TrackKind::Mic | TrackKind::ScreenAudio => AUDIO_STREAM_ID,
            TrackKind::Camera | TrackKind::Screen => VIDEO_STREAM_ID,
        }
    }

    /// Whether this is an audio track.
    pub fn is_audio(self) -> bool {
        matches!(self, TrackKind::Mic | TrackKind::ScreenAudio)
    }

    /// The codec capability this track is created with. Must stay in sync
    /// with the codecs registered in `crate::engine`.
    pub fn codec(self) -> RTCRtpCodecCapability {
        match self {
            TrackKind::Mic | TrackKind::ScreenAudio => RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: OPUS_FMTP.to_owned(),
                rtcp_feedback: vec![],
            },
            TrackKind::Camera => RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: H264_FMTP.to_owned(),
                rtcp_feedback: vec![],
            },
            TrackKind::Screen => RTCRtpCodecCapability {
                // The screen pipeline encodes H.264 (see screen_share.rs) — the
                // track MUST advertise the same codec or the RTP payloads are
                // unreadable on the receiving side.
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: H264_FMTP.to_owned(),
                rtcp_feedback: vec![],
            },
        }
    }
}

/// Opus fmtp: 20ms frames, in-band FEC, DTX (matches `engine.rs`).
pub(crate) const OPUS_FMTP: &str = "minptime=10;useinbandfec=1;usedtx=1";
/// H.264 fmtp: constrained baseline, packetization-mode 1 (matches `engine.rs`).
pub(crate) const H264_FMTP: &str =
    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";

/// The complete set of local tracks shared by all peer connections.
///
/// The mic track is always present; camera/screen tracks are created up front
/// but only added to peer connections while enabled, so toggling them triggers
/// per-peer renegotiation (fan-out lives in `crate::mesh`).
pub struct LocalTracks {
    pub mic: Arc<TrackLocalStaticSample>,
    pub camera: Arc<TrackLocalStaticSample>,
    pub screen: Arc<TrackLocalStaticSample>,
    pub screen_audio: Arc<TrackLocalStaticSample>,
}

impl Default for LocalTracks {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalTracks {
    /// Creates all four local tracks. Call once per `MeshManager`.
    pub fn new() -> Self {
        Self {
            mic: Arc::new(TrackLocalStaticSample::new(
                TrackKind::Mic.codec(),
                TrackKind::Mic.id().to_owned(),
                AUDIO_STREAM_ID.to_owned(),
            )),
            camera: Arc::new(TrackLocalStaticSample::new(
                TrackKind::Camera.codec(),
                TrackKind::Camera.id().to_owned(),
                VIDEO_STREAM_ID.to_owned(),
            )),
            screen: Arc::new(TrackLocalStaticSample::new(
                TrackKind::Screen.codec(),
                TrackKind::Screen.id().to_owned(),
                VIDEO_STREAM_ID.to_owned(),
            )),
            screen_audio: Arc::new(TrackLocalStaticSample::new(
                TrackKind::ScreenAudio.codec(),
                TrackKind::ScreenAudio.id().to_owned(),
                AUDIO_STREAM_ID.to_owned(),
            )),
        }
    }

    /// Returns the shared track for a kind.
    pub fn get(&self, kind: TrackKind) -> Arc<TrackLocalStaticSample> {
        match kind {
            TrackKind::Mic => Arc::clone(&self.mic),
            TrackKind::Camera => Arc::clone(&self.camera),
            TrackKind::Screen => Arc::clone(&self.screen),
            TrackKind::ScreenAudio => Arc::clone(&self.screen_audio),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
    use webrtc::track::track_local::TrackLocal;

    #[test]
    fn track_kind_metadata() {
        assert_eq!(TrackKind::Mic.id(), "mic");
        assert_eq!(TrackKind::ScreenAudio.id(), "screen-audio");
        assert!(TrackKind::Mic.is_audio());
        assert!(!TrackKind::Screen.is_audio());
        assert_eq!(TrackKind::Camera.stream_id(), VIDEO_STREAM_ID);
    }

    #[test]
    fn local_tracks_create_all_kinds() {
        let tracks = LocalTracks::new();
        for kind in TrackKind::ALL {
            let track = tracks.get(kind);
            let is_audio = track.kind() == RTPCodecType::Audio;
            assert_eq!(is_audio, kind.is_audio());
            assert_eq!(track.id(), kind.id());
        }
    }
}
