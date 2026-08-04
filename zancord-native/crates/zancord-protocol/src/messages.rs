//! Signaling protocol messages.
//!
//! All messages are adjacently tagged on the wire: `{"type": "...", "payload": {...}}`.
//! Unit variants (`LeaveRoom`, `RoomFull`) serialize with a `null` payload.

use serde::{Deserialize, Serialize};

use crate::room::{MediaStatePayload, PeerInfo};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum SignalMessage {
    // --- Room lifecycle ---
    JoinRoom {
        room_id: String,
        username: String,
    },
    LeaveRoom,
    RoomState {
        peers: Vec<PeerInfo>,
    },
    PeerJoined {
        peer: PeerInfo,
    },
    PeerLeft {
        peer_id: String,
    },
    RoomFull,

    // --- WebRTC signaling ---
    //
    // Directed messages carry both `target` (who receives it) and `sender`
    // (who sent it). `sender` is overwritten by the signaling server with the
    // authenticated connection id — clients must not trust its value, and the
    // receiving mesh uses it to route to the right peer connection.
    //
    // Renegotiate is a request from the non-offering peer to the offering
    // peer (lexicographically smaller id) to start a new offer/answer cycle
    // after a local track add/remove. webrtc-rs cannot roll back local offers,
    // so Zancord uses a single-offerer scheme instead of JSEP rollback: the
    // smaller-id peer is the only peer that ever creates offers, which makes
    // glare impossible by construction.
    Offer {
        target: String,
        sender: String,
        sdp: String,
    },
    Answer {
        target: String,
        sender: String,
        sdp: String,
    },
    IceCandidate {
        target: String,
        sender: String,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
    Renegotiate {
        target: String,
        sender: String,
    },

    // --- Media state ---
    MediaState {
        peer_id: String,
        state: MediaStatePayload,
    },

    // --- Chat ---
    ChatMessage {
        sender: String,
        content: String,
        timestamp: u64,
    },

    // --- Errors ---
    Error {
        code: String,
        message: String,
    },
}

impl SignalMessage {
    /// Returns `true` if this message should be relayed only to its `target` peer.
    pub fn is_directed(&self) -> bool {
        matches!(
            self,
            SignalMessage::Offer { .. }
                | SignalMessage::Answer { .. }
                | SignalMessage::IceCandidate { .. }
                | SignalMessage::Renegotiate { .. }
        )
    }

    /// The peer this message is directed at, if any.
    pub fn target(&self) -> Option<&str> {
        match self {
            SignalMessage::Offer { target, .. }
            | SignalMessage::Answer { target, .. }
            | SignalMessage::IceCandidate { target, .. }
            | SignalMessage::Renegotiate { target, .. } => Some(target),
            _ => None,
        }
    }
}
