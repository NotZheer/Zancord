//! Single peer connection manager (Phase 1D.2): ICE `ice_servers: []`
//! (Tailscale direct — no STUN/TURN), MaxBundle, local track plumbing, and
//! callbacks wired to signaling, media routing, and mesh events.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, warn};
use webrtc::api::API;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::bundle_policy::RTCBundlePolicy;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_sender::RTCRtpSender;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_remote::TrackRemote;

use zancord_protocol::{EncodedAudioFrame, EncodedVideoFrame, SignalMessage};

use crate::bridge;
use crate::mesh::MeshEvent;
use crate::negotiation::Negotiator;
use crate::rtcp::{spawn_rtcp_receive_loop, RtcpFeedback};
use crate::tracks::{LocalTracks, TrackKind};

/// A single full-mesh edge: one `RTCPeerConnection` to one remote peer.
pub struct PeerConnection {
    /// Remote peer id.
    pub peer_id: String,
    pc: Arc<RTCPeerConnection>,
    negotiator: Negotiator,
    /// Local track senders by kind (present = track currently added).
    senders: HashMap<TrackKind, Arc<RTCRtpSender>>,
    /// RTCP feedback channel to the app (PLI/REMB from remote senders).
    feedback_tx: broadcast::Sender<RtcpFeedback>,
    /// ICE candidates received before the remote description existed.
    pending_ice_candidates: Mutex<Vec<RTCIceCandidateInit>>,
}

impl PeerConnection {
    /// Creates the peer connection, wires every callback, and adds the local
    /// tracks that are currently enabled.
    ///
    /// - `camera` / `screen`: whether the camera / screen+screen-audio tracks
    ///   should be attached to this connection now.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        api: &API,
        local_id: String,
        peer_id: String,
        tracks: &LocalTracks,
        camera: bool,
        screen: bool,
        audio_sink: mpsc::Sender<EncodedAudioFrame>,
        screen_audio_sink: mpsc::Sender<EncodedAudioFrame>,
        video_sink: mpsc::Sender<EncodedVideoFrame>,
        feedback_tx: broadcast::Sender<RtcpFeedback>,
        event_tx: broadcast::Sender<MeshEvent>,
        signaling_tx: mpsc::Sender<SignalMessage>,
    ) -> Result<Self> {
        let config = RTCConfiguration {
            // Tailscale mesh: peers reach each other directly via Tailscale IPs.
            // No STUN/TURN — never.
            ice_servers: vec![],
            ice_transport_policy: RTCIceTransportPolicy::All,
            bundle_policy: RTCBundlePolicy::MaxBundle,
            ..Default::default()
        };
        let pc = Arc::new(api.new_peer_connection(config).await?);
        let negotiator = Negotiator::new(local_id.clone(), peer_id.clone(), Arc::clone(&pc));

        // Track negotiation: ALL offers/answers flow through the negotiator.
        {
            let negotiator = negotiator.clone();
            let signaling_tx = signaling_tx.clone();
            let peer_id = peer_id.clone();
            pc.on_negotiation_needed(Box::new(move || {
                let negotiator = negotiator.clone();
                let signaling_tx = signaling_tx.clone();
                let peer_id = peer_id.clone();
                Box::pin(async move {
                    if let Err(err) = negotiator.on_negotiation_needed(&signaling_tx).await {
                        warn!(peer = %peer_id, error = %err, "negotiation needed failed");
                    }
                })
            }));
        }

        // Trickle ICE: forward local candidates to the remote peer.
        {
            let signaling_tx = signaling_tx.clone();
            let peer_id = peer_id.clone();
            let local_id = local_id.clone();
            pc.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
                let signaling_tx = signaling_tx.clone();
                let peer_id = peer_id.clone();
                let local_id = local_id.clone();
                Box::pin(async move {
                    let Some(candidate) = candidate else {
                        return; // gathering complete
                    };
                    let Ok(init) = candidate.to_json() else {
                        warn!(peer = %peer_id, "failed to serialize local ice candidate");
                        return;
                    };
                    let _ = signaling_tx
                        .send(SignalMessage::IceCandidate {
                            target: peer_id,
                            sender: local_id.clone(),
                            candidate: init.candidate,
                            sdp_mid: init.sdp_mid,
                            sdp_mline_index: init.sdp_mline_index,
                        })
                        .await;
                })
            }));
        }

        // ICE state changes surface as mesh events.
        {
            let event_tx = event_tx.clone();
            let peer_id = peer_id.clone();
            pc.on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
                let event_tx = event_tx.clone();
                let peer_id = peer_id.clone();
                Box::pin(async move {
                    let _ = event_tx.send(MeshEvent::IceStateChanged {
                        peer_id,
                        state: state.into(),
                    });
                })
            }));
        }

        // Remote media: route by codec MIME (and track id for audio) into the
        // decode channels: `screen-audio` opus goes to the screen-audio sink.
        {
            let audio_sink = audio_sink.clone();
            let screen_audio_sink = screen_audio_sink.clone();
            let video_sink = video_sink.clone();
            let peer_id = peer_id.clone();
            pc.on_track(Box::new(
                move |track: Arc<TrackRemote>, _receiver, _transceiver| {
                    let audio_sink = audio_sink.clone();
                    let screen_audio_sink = screen_audio_sink.clone();
                    let video_sink = video_sink.clone();
                    let peer_id = peer_id.clone();
                    Box::pin(async move {
                        route_incoming_track(
                            &peer_id,
                            &track,
                            &audio_sink,
                            &screen_audio_sink,
                            &video_sink,
                        )
                        .await;
                    })
                },
            ));
        }

        let mut this = Self {
            peer_id,
            pc: Arc::clone(&pc),
            negotiator,
            senders: HashMap::new(),
            feedback_tx,
            pending_ice_candidates: Mutex::new(Vec::new()),
        };

        // Local tracks: mic is always attached; camera/screen per call flags.
        this.add_local_track(TrackKind::Mic, tracks.get(TrackKind::Mic))
            .await?;
        if camera {
            this.add_local_track(TrackKind::Camera, tracks.get(TrackKind::Camera))
                .await?;
        }
        if screen {
            this.add_local_track(TrackKind::Screen, tracks.get(TrackKind::Screen))
                .await?;
            this.add_local_track(TrackKind::ScreenAudio, tracks.get(TrackKind::ScreenAudio))
                .await?;
        }
        Ok(this)
    }

    /// The underlying peer connection (exposed for tests/observability).
    pub fn pc(&self) -> &Arc<RTCPeerConnection> {
        &self.pc
    }

    /// The perfect-negotiation manager for this peer (exposed for tests).
    pub fn negotiator(&self) -> &Negotiator {
        &self.negotiator
    }

    /// Attaches a local track to this connection; triggers renegotiation via
    /// the PC's negotiation-needed callback.
    pub async fn add_local_track(
        &mut self,
        kind: TrackKind,
        track: Arc<TrackLocalStaticSample>,
    ) -> Result<()> {
        if self.senders.contains_key(&kind) {
            return Ok(());
        }
        let sender = self.pc.add_track(track).await?;
        debug!(peer = %self.peer_id, ?kind, "local track attached");
        if !kind.is_audio() {
            spawn_rtcp_receive_loop(
                Arc::clone(&sender),
                self.peer_id.clone(),
                kind,
                self.feedback_tx.clone(),
            );
        }
        self.senders.insert(kind, sender);
        Ok(())
    }

    /// Detaches a local track; triggers renegotiation.
    pub async fn remove_local_track(&mut self, kind: TrackKind) -> Result<()> {
        let Some(sender) = self.senders.remove(&kind) else {
            return Ok(());
        };
        self.pc.remove_track(&sender).await?;
        debug!(peer = %self.peer_id, ?kind, "local track detached");
        Ok(())
    }

    /// `Negotiator::on_negotiation_needed` — only used by the mesh to kick the
    /// initial offer on the polite side.
    pub async fn negotiate(&self, signaling_tx: &mpsc::Sender<SignalMessage>) -> Result<()> {
        self.negotiator.on_negotiation_needed(signaling_tx).await
    }

    /// Remote offer → negotiation → answer via `signaling_tx`.
    pub async fn handle_offer(
        &self,
        sdp: String,
        signaling_tx: &mpsc::Sender<SignalMessage>,
    ) -> Result<()> {
        self.negotiator.handle_offer(sdp, signaling_tx).await?;
        self.flush_pending_ice_candidates().await;
        Ok(())
    }

    /// Remote `Renegotiate` request → the offerer starts a new offer cycle.
    pub async fn handle_renegotiate(
        &self,
        signaling_tx: &mpsc::Sender<SignalMessage>,
    ) -> Result<()> {
        self.negotiator.handle_renegotiate(signaling_tx).await
    }

    /// Remote answer.
    pub async fn handle_answer(&self, sdp: String) -> Result<()> {
        self.negotiator.handle_answer(sdp).await?;
        self.flush_pending_ice_candidates().await;
        Ok(())
    }

    /// Remote ICE candidate; buffered until the remote description is set.
    pub async fn handle_ice_candidate(
        &self,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    ) -> Result<()> {
        if self.pc.remote_description().await.is_none() {
            self.pending_ice_candidates
                .lock()
                .unwrap()
                .push(RTCIceCandidateInit {
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                    username_fragment: None,
                });
            return Ok(());
        }
        self.add_ice_candidate(candidate, sdp_mid, sdp_mline_index)
            .await
    }

    async fn add_ice_candidate(
        &self,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    ) -> Result<()> {
        let init = RTCIceCandidateInit {
            candidate,
            sdp_mid,
            sdp_mline_index,
            username_fragment: None,
        };
        if let Err(err) = self.pc.add_ice_candidate(init).await {
            // e.g. remote closed — nothing useful to do; the mesh removes us.
            warn!(peer = %self.peer_id, error = %err, "add_ice_candidate failed");
        }
        Ok(())
    }

    async fn flush_pending_ice_candidates(&self) {
        let pending = {
            let mut guard = self.pending_ice_candidates.lock().unwrap();
            std::mem::take(&mut *guard)
        };
        let count = pending.len();
        if count == 0 || self.pc.remote_description().await.is_none() {
            return;
        }
        for init in pending {
            let _ = self
                .add_ice_candidate(init.candidate, init.sdp_mid, init.sdp_mline_index)
                .await;
        }
        debug!(peer = %self.peer_id, count, "flushed buffered ice candidates");
    }

    /// Closes the peer connection (idempotent).
    pub async fn close(&self) -> Result<()> {
        self.pc.close().await?;
        Ok(())
    }
}

/// Which incoming channel an audio track belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioRoute {
    Mic,
    ScreenAudio,
}

/// Decides the audio route from the codec MIME and the remote track id.
/// The screen-audio track is `screen-audio`; every other audio track is the
/// peer's mic. Returns `None` for non-audio MIMEs.
fn route_audio(mime: &str, track_id: &str) -> Option<AudioRoute> {
    if !(mime.contains("opus") || mime.contains("audio/")) {
        return None;
    }
    if track_id == "screen-audio" {
        Some(AudioRoute::ScreenAudio)
    } else {
        Some(AudioRoute::Mic)
    }
}

/// Routes an incoming remote track to the right decode channel by codec MIME
/// and (for audio) track id.
async fn route_incoming_track(
    peer_id: &str,
    track: &Arc<TrackRemote>,
    audio_sink: &mpsc::Sender<EncodedAudioFrame>,
    screen_audio_sink: &mpsc::Sender<EncodedAudioFrame>,
    video_sink: &mpsc::Sender<EncodedVideoFrame>,
) {
    let mime = track.codec().capability.mime_type.to_lowercase();
    debug!(peer = %peer_id, id = %track.id(), mime = %mime, "incoming remote track");

    match route_audio(&mime, &track.id()) {
        Some(AudioRoute::ScreenAudio) => {
            tokio::spawn(bridge::audio_receive_loop(
                Arc::clone(track),
                screen_audio_sink.clone(),
            ));
        }
        Some(AudioRoute::Mic) => {
            tokio::spawn(bridge::audio_receive_loop(
                Arc::clone(track),
                audio_sink.clone(),
            ));
        }
        None if mime.contains("vp8") || mime.contains("h264") || mime.contains("video/") => {
            tokio::spawn(bridge::video_receive_loop(
                Arc::clone(track),
                video_sink.clone(),
            ));
        }
        None => {
            warn!(peer = %peer_id, id = %track.id(), mime = %mime, "unroutable incoming track");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;

    #[test]
    fn track_kind_matches_rtp_kind() {
        assert_eq!(
            TrackKind::Mic.codec().mime_type.to_lowercase(),
            "audio/opus"
        );
        assert_eq!(
            TrackKind::Camera.codec().mime_type.to_lowercase(),
            "video/h264"
        );
        assert_eq!(
            TrackKind::Screen.codec().mime_type.to_lowercase(),
            "video/vp8"
        );
    }

    #[test]
    fn routing_helpers_accept_known_mimes() {
        // Mirrors the dispatch in route_incoming_track (mime-level).
        let mime_route = |mime: &str| {
            if mime.contains("opus") || mime.contains("audio/") {
                "audio"
            } else if mime.contains("vp8") || mime.contains("h264") || mime.contains("video/") {
                "video"
            } else {
                "unknown"
            }
        };
        assert_eq!(mime_route("audio/opus"), "audio");
        assert_eq!(mime_route("video/VP8"), "video");
        assert_eq!(mime_route("video/H264"), "video");
        assert_eq!(mime_route("audio/G722"), "audio");
        assert_eq!(mime_route("application/rtx"), "unknown");
    }

    #[test]
    fn screen_audio_track_routes_to_its_own_sink() {
        // The screen-audio track id must win over the generic audio MIME route.
        assert_eq!(
            route_audio("audio/opus", "screen-audio"),
            Some(AudioRoute::ScreenAudio)
        );
        assert_eq!(route_audio("audio/opus", "mic"), Some(AudioRoute::Mic));
        assert_eq!(
            route_audio("audio/opus", "anything-else"),
            Some(AudioRoute::Mic)
        );
        assert_eq!(route_audio("video/h264", "screen"), None);
        assert_eq!(route_audio("audio/g722", "mic"), Some(AudioRoute::Mic));
    }

    #[test]
    fn rtp_codec_type_enum_is_stable() {
        // Sanity: RTPCodecType values used in dispatch.
        assert_eq!(RTPCodecType::Audio as u8, 1);
        assert_eq!(RTPCodecType::Video as u8, 2);
    }
}
