//! Full mesh manager (Phase 1D.4): peer lifecycle, local track fan-out,
//! signaling ↔ peer connection wiring, and the channel surface the app
//! (Phase 2 orchestrator) consumes.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use zancord_protocol::{
    EncodedAudioFrame, EncodedVideoFrame, MediaStatePayload, PeerInfo, SignalMessage,
};

use crate::bridge;
use crate::engine;
use crate::peer::PeerConnection;
use crate::rtcp::RtcpFeedback;
use crate::tracks::{LocalTracks, TrackKind};

/// Default max remote peers (5 remote + self = 6, the room capacity).
pub const DEFAULT_MAX_PEERS: usize = 5;

/// Events emitted by the mesh for the app/UI.
#[derive(Debug, Clone)]
pub enum MeshEvent {
    /// A new peer connection was created (before ICE completes).
    PeerConnected { peer_id: String },
    /// A peer connection was closed and removed.
    PeerDisconnected { peer_id: String },
    /// ICE transport state for a peer changed.
    IceStateChanged { peer_id: String, state: IceState },
    /// Remote peer media state passthrough (mic/camera/screen/deafen).
    MediaState {
        peer_id: String,
        state: MediaStatePayload,
    },
}

/// ICE transport state, mirrored from `RTCIceConnectionState` so the app does
/// not depend on webrtc types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceState {
    New,
    Checking,
    Connected,
    Completed,
    Disconnected,
    Failed,
    Closed,
}

impl From<webrtc::ice_transport::ice_connection_state::RTCIceConnectionState> for IceState {
    fn from(state: webrtc::ice_transport::ice_connection_state::RTCIceConnectionState) -> Self {
        use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState as S;
        match state {
            S::New => IceState::New,
            S::Checking => IceState::Checking,
            S::Connected => IceState::Connected,
            S::Completed => IceState::Completed,
            S::Disconnected => IceState::Disconnected,
            S::Failed => IceState::Failed,
            S::Closed => IceState::Closed,
            S::Unspecified => IceState::New,
        }
    }
}

/// Full-mesh WebRTC manager: one `PeerConnection` per remote peer.
///
/// Owned by a single app task (not `Sync`-shared). Signaling messages from
/// the signaling client are fed in via [`MeshManager::handle_signal`];
/// media flows over the exposed channel surface.
pub struct MeshManager {
    local_id: String,
    api: webrtc::api::API,
    peers: HashMap<String, PeerConnection>,
    signaling_tx: mpsc::Sender<SignalMessage>,
    max_peers: usize,
    tracks: LocalTracks,
    /// Local media enablement (camera/screen fan-out state).
    camera_enabled: bool,
    screen_enabled: bool,
    /// Send-side channels: the app pushes encoded frames here.
    audio_tx: mpsc::Sender<EncodedAudioFrame>,
    camera_tx: mpsc::Sender<EncodedVideoFrame>,
    screen_tx: mpsc::Sender<EncodedVideoFrame>,
    screen_audio_tx: mpsc::Sender<EncodedAudioFrame>,
    /// Receive-side channels per peer (receiver halves taken by the app).
    incoming_audio: HashMap<String, mpsc::Receiver<EncodedAudioFrame>>,
    incoming_screen_audio: HashMap<String, mpsc::Receiver<EncodedAudioFrame>>,
    incoming_video: HashMap<String, mpsc::Receiver<EncodedVideoFrame>>,
    /// Outgoing channels to the app.
    event_tx: broadcast::Sender<MeshEvent>,
    feedback_tx: broadcast::Sender<RtcpFeedback>,
    /// Directed signaling for peers not yet connected (buffered).
    pending_signals: HashMap<String, Vec<SignalMessage>>,
}

impl MeshManager {
    /// Creates the mesh and starts the send loops for the shared local tracks.
    pub fn new(
        local_id: String,
        signaling_tx: mpsc::Sender<SignalMessage>,
        max_peers: usize,
    ) -> Result<Self> {
        Self::with_api(local_id, signaling_tx, max_peers, engine::build_api()?)
    }

    /// Creates the mesh with a caller-provided API (used by tests).
    pub fn with_api(
        local_id: String,
        signaling_tx: mpsc::Sender<SignalMessage>,
        max_peers: usize,
        api: webrtc::api::API,
    ) -> Result<Self> {
        let tracks = LocalTracks::new();

        let (audio_tx, audio_rx) = mpsc::channel(256);
        let (camera_tx, camera_rx) = mpsc::channel(64);
        let (screen_tx, screen_rx) = mpsc::channel(64);
        let (screen_audio_tx, screen_audio_rx) = mpsc::channel(256);
        tokio::spawn(bridge::audio_send_loop(tracks.mic.clone(), audio_rx));
        tokio::spawn(bridge::video_send_loop(tracks.camera.clone(), camera_rx));
        tokio::spawn(bridge::video_send_loop(tracks.screen.clone(), screen_rx));
        tokio::spawn(bridge::audio_send_loop(
            tracks.screen_audio.clone(),
            screen_audio_rx,
        ));

        let (event_tx, _) = broadcast::channel(32);
        let (feedback_tx, _) = broadcast::channel(32);

        info!(local = %local_id, max_peers, "mesh manager created");
        Ok(Self {
            local_id,
            api,
            peers: HashMap::new(),
            signaling_tx,
            max_peers,
            tracks,
            camera_enabled: false,
            screen_enabled: false,
            audio_tx,
            camera_tx,
            screen_tx,
            screen_audio_tx,
            incoming_audio: HashMap::new(),
            incoming_screen_audio: HashMap::new(),
            incoming_video: HashMap::new(),
            event_tx,
            feedback_tx,
            pending_signals: HashMap::new(),
        })
    }

    pub fn local_id(&self) -> &str {
        &self.local_id
    }

    /// Number of connected remote peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Max remote peers this mesh will connect.
    pub fn max_peers(&self) -> usize {
        self.max_peers
    }

    /// Sorted remote peer ids.
    pub fn peer_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.peers.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn is_peer_connected(&self, peer_id: &str) -> bool {
        self.peers.contains_key(peer_id)
    }

    /// The mesh's outgoing signaling channel (for the signaling client).
    pub fn signaling_tx(&self) -> mpsc::Sender<SignalMessage> {
        self.signaling_tx.clone()
    }

    /// Channel the audio pipeline pushes encoded Opus frames into (fan-out to
    /// every peer's audio track).
    pub fn audio_tx(&self) -> mpsc::Sender<EncodedAudioFrame> {
        self.audio_tx.clone()
    }

    /// Channel the screen-audio capture pushes encoded Opus frames into
    /// (fan-out to every peer's screen-audio track).
    pub fn screen_audio_tx(&self) -> mpsc::Sender<EncodedAudioFrame> {
        self.screen_audio_tx.clone()
    }

    /// Channel the camera pipeline pushes encoded video frames into.
    pub fn camera_tx(&self) -> mpsc::Sender<EncodedVideoFrame> {
        self.camera_tx.clone()
    }

    /// Channel the screen-capture pipeline pushes encoded video frames into.
    pub fn screen_tx(&self) -> mpsc::Sender<EncodedVideoFrame> {
        self.screen_tx.clone()
    }

    /// Subscribe to mesh lifecycle / media-state events.
    pub fn event_rx(&self) -> broadcast::Receiver<MeshEvent> {
        self.event_tx.subscribe()
    }

    /// Subscribe to RTCP feedback (keyframe requests, bitrate hints).
    pub fn feedback_rx(&self) -> broadcast::Receiver<RtcpFeedback> {
        self.feedback_tx.subscribe()
    }

    /// Takes the per-peer incoming audio channel (Opus frames from `peer_id`).
    /// Returns `None` if already taken or the peer is unknown.
    pub fn take_incoming_audio(
        &mut self,
        peer_id: &str,
    ) -> Option<mpsc::Receiver<EncodedAudioFrame>> {
        self.incoming_audio.remove(peer_id)
    }

    /// Takes the per-peer incoming screen-audio channel (stereo Opus frames
    /// from `peer_id`'s screen share). Returns `None` if already taken or the
    /// peer is unknown.
    pub fn take_incoming_screen_audio(
        &mut self,
        peer_id: &str,
    ) -> Option<mpsc::Receiver<EncodedAudioFrame>> {
        self.incoming_screen_audio.remove(peer_id)
    }

    /// Takes the per-peer incoming video channel (RTP payloads from `peer_id`).
    pub fn take_incoming_video(
        &mut self,
        peer_id: &str,
    ) -> Option<mpsc::Receiver<EncodedVideoFrame>> {
        self.incoming_video.remove(peer_id)
    }

    /// Toggles the local camera track on every peer connection (fan-out).
    /// Adding/removing triggers per-peer renegotiation.
    pub async fn set_camera_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.camera_enabled == enabled {
            return Ok(());
        }
        self.camera_enabled = enabled;
        self.fan_out_track(TrackKind::Camera, enabled).await
    }

    /// Toggles the local screen + screen-audio tracks on every peer
    /// connection (fan-out).
    pub async fn set_screen_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.screen_enabled == enabled {
            return Ok(());
        }
        self.screen_enabled = enabled;
        self.fan_out_track(TrackKind::Screen, enabled).await?;
        self.fan_out_track(TrackKind::ScreenAudio, enabled).await
    }

    pub fn camera_enabled(&self) -> bool {
        self.camera_enabled
    }

    pub fn screen_enabled(&self) -> bool {
        self.screen_enabled
    }

    async fn fan_out_track(&mut self, kind: TrackKind, enabled: bool) -> Result<()> {
        let track = self.tracks.get(kind);
        for peer_id in self.peer_ids() {
            let peer = self.peers.get_mut(&peer_id).expect("peer from peer_ids");
            let result = if enabled {
                peer.add_local_track(kind, std::sync::Arc::clone(&track))
                    .await
            } else {
                peer.remove_local_track(kind).await
            };
            if let Err(err) = result {
                warn!(peer = %peer_id, ?kind, error = %err, "track fan-out failed");
            }
        }
        Ok(())
    }

    /// Creates a peer connection for a newly joined peer. The polite side
    /// (lexicographically smaller id) sends the initial offer.
    pub async fn handle_peer_joined(&mut self, peer: PeerInfo) -> Result<()> {
        if self.peers.contains_key(&peer.id) {
            debug!(peer = %peer.id, "peer already connected, ignoring PeerJoined");
            return Ok(());
        }
        if self.peers.len() >= self.max_peers {
            warn!(peer = %peer.id, "mesh at capacity, ignoring PeerJoined");
            return Err(anyhow!("mesh at capacity ({} peers)", self.max_peers));
        }

        let (audio_sink, audio_rx) = mpsc::channel(256);
        let (screen_audio_sink, screen_audio_rx) = mpsc::channel(64);
        let (video_sink, video_rx) = mpsc::channel(64);

        let pc = PeerConnection::new(
            &self.api,
            self.local_id.clone(),
            peer.id.clone(),
            &self.tracks,
            self.camera_enabled,
            self.screen_enabled,
            audio_sink,
            screen_audio_sink,
            video_sink,
            self.feedback_tx.clone(),
            self.event_tx.clone(),
            self.signaling_tx.clone(),
        )
        .await?;

        self.incoming_audio.insert(peer.id.clone(), audio_rx);
        self.incoming_screen_audio
            .insert(peer.id.clone(), screen_audio_rx);
        self.incoming_video.insert(peer.id.clone(), video_rx);
        self.peers.insert(peer.id.clone(), pc);
        let _ = self.event_tx.send(MeshEvent::PeerConnected {
            peer_id: peer.id.clone(),
        });
        info!(local = %self.local_id, peer = %peer.id, count = self.peers.len(), "peer connected");

        // The offerer (smaller id) initiates; the other side answers.
        if self.local_id < peer.id {
            let pc = self.peers.get(&peer.id).expect("peer just inserted");
            pc.negotiate(&self.signaling_tx).await?;
        }

        // Flush any signaling that arrived before this peer was connected.
        if let Some(messages) = self.pending_signals.remove(&peer.id) {
            for msg in messages {
                if let Err(err) = self.handle_signal(msg).await {
                    warn!(peer = %peer.id, error = %err, "buffered signal failed");
                }
            }
        }
        Ok(())
    }

    /// Closes and removes the peer connection for a departed peer.
    pub async fn handle_peer_left(&mut self, peer_id: &str) -> Result<()> {
        let Some(peer) = self.peers.remove(peer_id) else {
            return Ok(());
        };
        if let Err(err) = peer.close().await {
            warn!(peer = %peer_id, error = %err, "peer close failed");
        }
        self.incoming_audio.remove(peer_id);
        self.incoming_screen_audio.remove(peer_id);
        self.incoming_video.remove(peer_id);
        self.pending_signals.remove(peer_id);
        let _ = self.event_tx.send(MeshEvent::PeerDisconnected {
            peer_id: peer_id.to_owned(),
        });
        info!(local = %self.local_id, peer = %peer_id, count = self.peers.len(), "peer disconnected");
        Ok(())
    }

    /// Feeds one signaling message from the signaling client into the mesh.
    /// Directed messages for unknown peers are buffered until they join.
    pub async fn handle_signal(&mut self, msg: SignalMessage) -> Result<()> {
        // Messages not addressed to us (echoes of our own sends) are ignored.
        if let Some(target) = msg.target() {
            if target != self.local_id {
                return Ok(());
            }
        }

        match msg {
            SignalMessage::Offer { sender, sdp, .. } => {
                if !self.peers.contains_key(&sender) {
                    let peer_id = sender.clone();
                    self.buffer_for(
                        &peer_id,
                        SignalMessage::Offer {
                            target: self.local_id.clone(),
                            sender,
                            sdp,
                        },
                    );
                    return Ok(());
                }
                let peer = self.peers.get(&sender).expect("checked above");
                peer.handle_offer(sdp, &self.signaling_tx).await?;
            }
            SignalMessage::Answer { sender, sdp, .. } => {
                if !self.peers.contains_key(&sender) {
                    let peer_id = sender.clone();
                    self.buffer_for(
                        &peer_id,
                        SignalMessage::Answer {
                            target: self.local_id.clone(),
                            sender,
                            sdp,
                        },
                    );
                    return Ok(());
                }
                let peer = self.peers.get(&sender).expect("checked above");
                peer.handle_answer(sdp).await?;
            }
            SignalMessage::IceCandidate {
                sender,
                candidate,
                sdp_mid,
                sdp_mline_index,
                ..
            } => {
                if !self.peers.contains_key(&sender) {
                    let peer_id = sender.clone();
                    self.buffer_for(
                        &peer_id,
                        SignalMessage::IceCandidate {
                            target: self.local_id.clone(),
                            sender,
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                        },
                    );
                    return Ok(());
                }
                let peer = self.peers.get(&sender).expect("checked above");
                peer.handle_ice_candidate(candidate, sdp_mid, sdp_mline_index)
                    .await?;
            }
            SignalMessage::Renegotiate { sender, .. } => {
                if !self.peers.contains_key(&sender) {
                    let peer_id = sender;
                    self.buffer_for(
                        &peer_id,
                        SignalMessage::Renegotiate {
                            target: self.local_id.clone(),
                            sender: peer_id.clone(),
                        },
                    );
                    return Ok(());
                }
                let peer = self.peers.get(&sender).expect("checked above");
                peer.handle_renegotiate(&self.signaling_tx).await?;
            }
            SignalMessage::MediaState { peer_id, state } => {
                let _ = self.event_tx.send(MeshEvent::MediaState { peer_id, state });
            }
            // Room lifecycle / chat messages are not transport concerns.
            other => {
                debug!(?other, "ignoring non-transport signal");
            }
        }
        Ok(())
    }

    fn buffer_for(&mut self, peer_id: &str, msg: SignalMessage) {
        debug!(peer = %peer_id, "buffering signal for unconnected peer");
        self.pending_signals
            .entry(peer_id.to_owned())
            .or_default()
            .push(msg);
    }

    /// Closes every peer connection (app shutdown).
    pub async fn shutdown(&mut self) -> Result<()> {
        let peers: Vec<String> = self.peers.keys().cloned().collect();
        for peer_id in peers {
            self.handle_peer_left(&peer_id).await?;
        }
        info!(local = %self.local_id, "mesh shut down");
        Ok(())
    }
}
