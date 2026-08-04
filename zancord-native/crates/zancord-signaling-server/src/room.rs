//! Room state management (Phase 1A.2): `RoomManager` with `Arc<RwLock<HashMap>>`,
//! max 6 peers, join/leave lifecycle, broadcast fan-out.
//!
//! Fan-out uses `tokio::sync::broadcast` (NOT per-peer mpsc): every event
//! carries the sender's peer id (`RoomEvent::sender`), and each connection
//! task drops events that are its own or directed at another peer. This makes
//! "clients filter their own messages" explicit in the design.

use std::collections::HashMap;

use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;
use zancord_protocol::room::{is_valid_room_id, sanitize_username};
use zancord_protocol::{MediaStatePayload, PeerId, PeerInfo, SignalMessage, MAX_ROOM_SIZE};

/// A peer currently connected to a room.
///
/// The plan's "info + sender" is unnecessary here: there is no per-peer
/// delivery sink. Every message fans out through the room's broadcast channel
/// and the receiving connection task filters by sender/target.
pub type ConnectedPeer = PeerInfo;

/// An event published on a room's broadcast channel.
///
/// `sender` lets receivers drop their own messages (`sender == my_id`) and
/// lets them drop directed messages aimed at other peers.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomEvent {
    pub sender: PeerId,
    pub message: SignalMessage,
}

/// Broadcast channel capacity per room. Plenty for rate-limited signaling
/// (30 msg/s/peer × 6 peers) plus headroom for bursty chat.
const CHANNEL_CAPACITY: usize = 128;

#[derive(Debug)]
struct Room {
    peers: HashMap<PeerId, ConnectedPeer>,
    tx: broadcast::Sender<RoomEvent>,
}

impl Room {
    fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            peers: HashMap::new(),
            tx,
        }
    }
}

/// Errors that prevent a peer from joining a room.
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    #[error("room '{0}' is at capacity ({MAX_ROOM_SIZE} peers)")]
    RoomFull(String),
    #[error("invalid room id '{0}'")]
    InvalidRoomId(String),
}

/// Outcome of a successful join.
#[derive(Debug)]
pub struct JoinInfo {
    /// The newly assigned peer (id, sanitized username, default media state).
    pub peer: ConnectedPeer,
    /// `RoomState` snapshot to send to the joiner (includes the joiner, so it
    /// can learn its own peer id).
    pub room_state: SignalMessage,
    /// Subscription to the room's broadcast channel for the new connection.
    pub events: broadcast::Receiver<RoomEvent>,
}

/// In-memory room registry. One instance per server process.
#[derive(Debug, Default)]
pub struct RoomManager {
    rooms: RwLock<HashMap<String, Room>>,
}

impl RoomManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Joins `username` to `room_id`, assigning a fresh uuid peer id.
    ///
    /// The subscription is created *before* `PeerJoined` is published, so the
    /// joiner's task cannot miss a subsequent event. The joiner receives its
    /// own `PeerJoined` on the subscription but filters it (sender == self).
    pub async fn join(&self, room_id: &str, username: &str) -> Result<JoinInfo, JoinError> {
        if !is_valid_room_id(room_id) {
            return Err(JoinError::InvalidRoomId(room_id.to_string()));
        }
        let username = sanitize_username(username);
        let id = Uuid::new_v4().to_string();

        let mut rooms = self.rooms.write().await;
        let room = rooms.entry(room_id.to_string()).or_insert_with(Room::new);
        if room.peers.len() >= MAX_ROOM_SIZE {
            return Err(JoinError::RoomFull(room_id.to_string()));
        }

        let events = room.tx.subscribe();
        let peer = ConnectedPeer {
            id: id.clone(),
            username,
            media_state: MediaStatePayload::default(),
        };
        let mut peers: Vec<PeerInfo> = room.peers.values().cloned().collect();
        peers.push(peer.clone());
        let room_state = SignalMessage::RoomState { peers };

        room.peers.insert(id.clone(), peer.clone());
        let _ = room.tx.send(RoomEvent {
            sender: id.clone(),
            message: SignalMessage::PeerJoined { peer: peer.clone() },
        });
        Ok(JoinInfo {
            peer,
            room_state,
            events,
        })
    }

    /// Removes `peer_id` from `room_id`, broadcasting `PeerLeft`, and deletes
    /// the room once it is empty.
    pub async fn leave(&self, room_id: &str, peer_id: &str) {
        let mut rooms = self.rooms.write().await;
        let Some(room) = rooms.get_mut(room_id) else {
            return;
        };
        let removed = room.peers.remove(peer_id).is_some();
        let empty = room.peers.is_empty();
        let tx = room.tx.clone();
        if empty {
            rooms.remove(room_id);
        }
        if removed {
            let _ = tx.send(RoomEvent {
                sender: peer_id.to_string(),
                message: SignalMessage::PeerLeft {
                    peer_id: peer_id.to_string(),
                },
            });
        }
    }

    /// Publishes an event to every subscriber in the room. Returns `false` if
    /// the room no longer exists (the message is dropped).
    pub async fn publish(&self, room_id: &str, sender: &str, message: SignalMessage) -> bool {
        let rooms = self.rooms.read().await;
        let Some(room) = rooms.get(room_id) else {
            return false;
        };
        room.tx
            .send(RoomEvent {
                sender: sender.to_string(),
                message,
            })
            .is_ok()
    }

    /// Updates a peer's stored media state (keeps `RoomState` snapshots fresh
    /// for late joiners). Returns `false` if the room or peer doesn't exist.
    pub async fn update_media_state(
        &self,
        room_id: &str,
        peer_id: &str,
        state: MediaStatePayload,
    ) -> bool {
        let mut rooms = self.rooms.write().await;
        let Some(room) = rooms.get_mut(room_id) else {
            return false;
        };
        let Some(peer) = room.peers.get_mut(peer_id) else {
            return false;
        };
        peer.media_state = state;
        true
    }

    /// Number of peers in `room_id`, or `None` if the room doesn't exist.
    pub async fn peer_count(&self, room_id: &str) -> Option<usize> {
        self.rooms.read().await.get(room_id).map(|r| r.peers.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::error::RecvError;

    #[tokio::test]
    async fn join_assigns_id_and_returns_room_state() {
        let m = RoomManager::new();
        let info = m.join("room1", "  alice  ").await.unwrap();
        assert_eq!(info.peer.username, "alice"); // sanitized
        assert_eq!(info.peer.media_state, MediaStatePayload::default());
        match info.room_state {
            SignalMessage::RoomState { peers } => {
                assert_eq!(peers.len(), 1);
                assert_eq!(peers[0].id, info.peer.id);
            }
            other => panic!("expected RoomState, got {other:?}"),
        }
        assert_eq!(m.peer_count("room1").await, Some(1));
    }

    #[tokio::test]
    async fn second_join_broadcasts_peer_joined() {
        let m = RoomManager::new();
        let alice = m.join("r", "alice").await.unwrap();
        let mut alice_events = alice.events;
        let bob = m.join("r", "bob").await.unwrap();

        // The joiner receives its own PeerJoined broadcast and filters it out
        // (broadcasts carry the sender id exactly for this reason).
        let own = alice_events.recv().await.unwrap();
        assert_eq!(own.sender, alice.peer.id);

        let ev = alice_events.recv().await.unwrap();
        assert_eq!(ev.sender, bob.peer.id);
        match ev.message {
            SignalMessage::PeerJoined { peer } => assert_eq!(peer.username, "bob"),
            other => panic!("expected PeerJoined, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn room_capacity_is_enforced() {
        let m = RoomManager::new();
        for i in 0..MAX_ROOM_SIZE {
            m.join("cap", &format!("user{i}")).await.unwrap();
        }
        let err = m.join("cap", "overflow").await.unwrap_err();
        assert!(matches!(err, JoinError::RoomFull(_)));
        assert_eq!(m.peer_count("cap").await, Some(MAX_ROOM_SIZE));
    }

    #[tokio::test]
    async fn leave_broadcasts_peer_left_and_deletes_empty_room() {
        let m = RoomManager::new();
        let alice = m.join("r", "alice").await.unwrap();
        let mut alice_events = alice.events;
        let bob = m.join("r", "bob").await.unwrap();
        let mut bob_events = bob.events;
        let _ = alice_events.recv().await.unwrap(); // own PeerJoined(alice)
        let _ = alice_events.recv().await.unwrap(); // PeerJoined(bob)

        m.leave("r", &bob.peer.id).await;
        let ev = alice_events.recv().await.unwrap();
        assert_eq!(ev.sender, bob.peer.id);
        assert_eq!(
            ev.message,
            SignalMessage::PeerLeft {
                peer_id: bob.peer.id.clone()
            }
        );
        assert_eq!(m.peer_count("r").await, Some(1));

        m.leave("r", &alice.peer.id).await;
        assert_eq!(m.peer_count("r").await, None);
        // b's subscription sees PeerJoined(bob), PeerLeft(bob), PeerLeft(alice),
        // then the room channel closes once the room is deleted.
        let _ = bob_events.recv().await.unwrap(); // own PeerJoined(bob)
        let _ = bob_events.recv().await.unwrap(); // PeerLeft(bob)
        let _ = bob_events.recv().await.unwrap(); // PeerLeft(alice)
        assert!(matches!(bob_events.recv().await, Err(RecvError::Closed)));
    }

    #[tokio::test]
    async fn invalid_room_id_is_rejected() {
        let m = RoomManager::new();
        let err = m.join("has space", "x").await.unwrap_err();
        assert!(matches!(err, JoinError::InvalidRoomId(_)));
    }

    #[tokio::test]
    async fn publish_reaches_all_subscribers() {
        let m = RoomManager::new();
        let alice = m.join("r", "alice").await.unwrap();
        let bob = m.join("r", "bob").await.unwrap();
        let mut alice_events = alice.events;
        let mut bob_events = bob.events;
        let _ = alice_events.recv().await.unwrap(); // own PeerJoined(alice)
        let _ = alice_events.recv().await.unwrap(); // PeerJoined(bob)
        let _ = bob_events.recv().await.unwrap(); // own PeerJoined(bob)

        let chat = SignalMessage::ChatMessage {
            sender: "alice".into(),
            content: "hi".into(),
            timestamp: 1,
        };
        assert!(m.publish("r", &alice.peer.id, chat.clone()).await);
        assert_eq!(alice_events.recv().await.unwrap().message, chat);
        assert_eq!(bob_events.recv().await.unwrap().message, chat);
        assert!(!m.publish("missing", &alice.peer.id, chat).await);
    }

    #[tokio::test]
    async fn media_state_updates_stored_peer() {
        let m = RoomManager::new();
        let alice = m.join("r", "alice").await.unwrap();
        let state = MediaStatePayload {
            mic_enabled: true,
            ..Default::default()
        };
        assert!(m.update_media_state("r", &alice.peer.id, state).await);
        assert!(!m.update_media_state("r", "ghost", state).await);

        let bob = m.join("r", "bob").await.unwrap();
        match bob.room_state {
            SignalMessage::RoomState { peers } => {
                let alice_in_state = peers.iter().find(|p| p.id == alice.peer.id).unwrap();
                assert!(alice_in_state.media_state.mic_enabled);
            }
            other => panic!("expected RoomState, got {other:?}"),
        }
    }
}
