//! WebSocket message routing & signal relay (Phase 1A.3): parse `SignalMessage`,
//! route directed messages to target peer, broadcast chat/media state.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};
use zancord_protocol::room::is_valid_room_id;
use zancord_protocol::SignalMessage;

use crate::rate_limit::{MessageKind, RateLimiter};
use crate::room::{JoinError, RoomManager};

/// How long a fresh connection may take to send its `JoinRoom` message.
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP handler for `GET /ws/:room_id`: validates the room id, then upgrades.
pub async fn ws_handler(
    State(manager): State<Arc<RoomManager>>,
    Path(room_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if !is_valid_room_id(&room_id) {
        warn!(target: "zancord_signaling_server", room_id = %room_id, "rejected connection: invalid room id");
        return StatusCode::BAD_REQUEST.into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, room_id, manager))
}

/// Full per-connection lifecycle: join handshake, then the relay loop, then
/// cleanup. Never panics on malformed input — worst case the connection ends.
async fn handle_socket(mut socket: WebSocket, room_id: String, manager: Arc<RoomManager>) {
    let mut limiter = RateLimiter::new();

    let Some(username) = receive_join(&mut socket, &mut limiter, &room_id).await else {
        return;
    };

    let (my_id, mut events) = match manager.join(&room_id, &username).await {
        Ok(info) => {
            let my_id = info.peer.id.clone();
            if socket
                .send(Message::Text(to_json(&info.room_state)))
                .await
                .is_err()
            {
                return;
            }
            info!(target: "zancord_signaling_server", room_id = %room_id, peer_id = %my_id, "peer joined room");
            (my_id, info.events)
        }
        Err(JoinError::RoomFull(_)) => {
            let _ = socket
                .send(Message::Text(to_json(&SignalMessage::RoomFull)))
                .await;
            return;
        }
        Err(JoinError::InvalidRoomId(_)) => {
            let _ = socket
                .send(Message::Text(to_json(&SignalMessage::Error {
                    code: "invalid_room".into(),
                    message: "invalid room id".into(),
                })))
                .await;
            return;
        }
    };

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_)) => {
                        debug!(target: "zancord_signaling_server", peer_id = %my_id, "socket recv ended");
                        break;
                    }
                    Some(Ok(Message::Text(text))) => {
                        if !handle_client_message(&mut socket, &manager, &room_id, &my_id, &mut limiter, &text).await {
                            // LeaveRoom: close the handshake cleanly so the peer
                            // sees a normal close instead of a reset.
                            let _ = socket.send(Message::Close(None)).await;
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Binary(_) | Message::Pong(_))) => {}
                }
            }
            event = events.recv() => {
                match event {
                    Ok(ev) if should_deliver(&ev.sender, &my_id, &ev.message) => {
                        if socket.send(Message::Text(to_json(&ev.message))).await.is_err() { break; }
                    }
                    Ok(_) => {} // own message, or not directed at us
                    Err(RecvError::Lagged(_)) => {
                        debug!(target: "zancord_signaling_server", peer_id = %my_id, "broadcast lag: dropped events");
                    }
                    Err(RecvError::Closed) => {
                        debug!(target: "zancord_signaling_server", peer_id = %my_id, "room channel closed");
                        break;
                    }
                }
            }
        }
    }

    manager.leave(&room_id, &my_id).await;
    info!(target: "zancord_signaling_server", room_id = %room_id, peer_id = %my_id, "peer left room");
}

/// Waits for the mandatory `JoinRoom` first message and rate-checks it.
/// Returns the (sanitized later) username, or `None` when the connection must
/// be dropped (timeout, protocol error, rate limit, room mismatch).
async fn receive_join(
    socket: &mut WebSocket,
    limiter: &mut RateLimiter,
    path_room: &str,
) -> Option<String> {
    loop {
        let text = match tokio::time::timeout(JOIN_TIMEOUT, socket.recv()).await {
            Ok(Some(Ok(Message::Text(t)))) => t,
            Ok(Some(Ok(Message::Ping(payload)))) => {
                let _ = socket.send(Message::Pong(payload)).await;
                continue;
            }
            Ok(Some(Ok(_))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => return None,
        };

        match serde_json::from_str::<SignalMessage>(&text) {
            Ok(SignalMessage::JoinRoom { room_id, username }) => {
                if !limiter.allow(MessageKind::Join) {
                    send_error(
                        socket,
                        "rate_limited",
                        "join rate limit exceeded (3 per 10s)",
                    )
                    .await;
                    return None;
                }
                if room_id != path_room {
                    send_error(
                        socket,
                        "room_mismatch",
                        &format!("this connection is for room '{path_room}', not '{room_id}'"),
                    )
                    .await;
                    return None;
                }
                return Some(username);
            }
            Ok(_) => {
                send_error(socket, "protocol_error", "first message must be JoinRoom").await;
                return None;
            }
            Err(e) => {
                warn!(target: "zancord_signaling_server", error = %e, "malformed join message");
                send_error(socket, "malformed", "malformed JSON").await;
                return None;
            }
        }
    }
}

/// Outcome of classifying one client message.
#[derive(Debug)]
enum Outcome {
    /// Accepted; relay/publish as appropriate.
    Continue,
    /// The peer wants to leave; close the connection.
    Leave,
    /// Rejected by the rate limiter; an `Error` is sent back.
    RateLimited(MessageKind),
}

/// Classifies a client message against the rate limiter (pure + sync so unit
/// tests can drive it directly). `LeaveRoom` is never rate-limited.
fn classify(limiter: &mut RateLimiter, msg: &SignalMessage) -> Outcome {
    if matches!(msg, SignalMessage::LeaveRoom) {
        return Outcome::Leave;
    }
    let kind = match msg {
        SignalMessage::ChatMessage { .. } => MessageKind::Chat,
        SignalMessage::MediaState { .. } => MessageKind::State,
        _ => MessageKind::Signal,
    };
    if limiter.allow(kind) {
        Outcome::Continue
    } else {
        Outcome::RateLimited(kind)
    }
}

fn rate_limit_message(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Signal => "signal rate limit exceeded (30/s)",
        MessageKind::Chat => "chat rate limit exceeded (5/s)",
        MessageKind::State => "media state rate limit exceeded (10/s)",
        MessageKind::Join => "join rate limit exceeded (3 per 10s)",
    }
}

/// Handles one text message from a peer. Returns `false` when the peer wants
/// to leave or the connection died.
async fn handle_client_message(
    socket: &mut WebSocket,
    manager: &RoomManager,
    room_id: &str,
    my_id: &str,
    limiter: &mut RateLimiter,
    text: &str,
) -> bool {
    let msg: SignalMessage = match serde_json::from_str(text) {
        Ok(msg) => msg,
        Err(e) => {
            warn!(target: "zancord_signaling_server", peer_id = %my_id, error = %e, "malformed message ignored");
            return true;
        }
    };

    match classify(limiter, &msg) {
        Outcome::RateLimited(kind) => {
            let _ = socket
                .send(Message::Text(to_json(&SignalMessage::Error {
                    code: "rate_limited".into(),
                    message: rate_limit_message(kind).into(),
                })))
                .await;
            return true;
        }
        Outcome::Leave => return false,
        Outcome::Continue => {}
    }

    match msg {
        SignalMessage::MediaState { peer_id, state } => {
            // Ignore spoofed ownership: only the owner may update their entry.
            if peer_id == my_id {
                manager.update_media_state(room_id, my_id, state).await;
            }
            manager
                .publish(
                    room_id,
                    my_id,
                    SignalMessage::MediaState {
                        peer_id: my_id.to_string(),
                        state,
                    },
                )
                .await;
        }
        SignalMessage::ChatMessage {
            content, timestamp, ..
        } => {
            // Re-broadcast with the authenticated sender id.
            manager
                .publish(
                    room_id,
                    my_id,
                    SignalMessage::ChatMessage {
                        sender: my_id.to_string(),
                        content,
                        timestamp,
                    },
                )
                .await;
        }
        SignalMessage::Offer { target, sdp, .. } => {
            // Authenticated sender: never trust the client-supplied value.
            manager
                .publish(
                    room_id,
                    my_id,
                    SignalMessage::Offer {
                        target,
                        sender: my_id.to_string(),
                        sdp,
                    },
                )
                .await;
        }
        SignalMessage::Answer { target, sdp, .. } => {
            manager
                .publish(
                    room_id,
                    my_id,
                    SignalMessage::Answer {
                        target,
                        sender: my_id.to_string(),
                        sdp,
                    },
                )
                .await;
        }
        SignalMessage::IceCandidate {
            target,
            candidate,
            sdp_mid,
            sdp_mline_index,
            ..
        } => {
            manager
                .publish(
                    room_id,
                    my_id,
                    SignalMessage::IceCandidate {
                        target,
                        sender: my_id.to_string(),
                        candidate,
                        sdp_mid,
                        sdp_mline_index,
                    },
                )
                .await;
        }
        SignalMessage::Renegotiate { target, .. } => {
            manager
                .publish(
                    room_id,
                    my_id,
                    SignalMessage::Renegotiate {
                        target,
                        sender: my_id.to_string(),
                    },
                )
                .await;
        }
        other => {
            warn!(target: "zancord_signaling_server", peer_id = %my_id, ?other, "unexpected message ignored")
        }
    }
    true
}

/// Decides whether `ev` should be forwarded to the connection of `my_id`.
pub(crate) fn should_deliver(sender: &str, my_id: &str, msg: &SignalMessage) -> bool {
    if sender == my_id {
        return false; // never echo a peer's own messages back to it
    }
    if msg.is_directed() {
        return msg.target() == Some(my_id); // directed messages go only to their target
    }
    true
}

/// Serializes a message for the wire. Protocol messages are plain data, so
/// serialization cannot fail.
fn to_json(msg: &SignalMessage) -> String {
    serde_json::to_string(msg).expect("SignalMessage serialization is infallible")
}

async fn send_error(socket: &mut WebSocket, code: &str, message: &str) {
    let _ = socket
        .send(Message::Text(to_json(&SignalMessage::Error {
            code: code.into(),
            message: message.into(),
        })))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_broadcasts_are_dropped() {
        let msg = SignalMessage::ChatMessage {
            sender: "me".into(),
            content: "hi".into(),
            timestamp: 0,
        };
        assert!(!should_deliver("me", "me", &msg));
        assert!(should_deliver("other", "me", &msg));
    }

    #[test]
    fn directed_messages_only_reach_their_target() {
        let offer = SignalMessage::Offer {
            target: "bob".into(),
            sender: "alice".into(),
            sdp: "v=0".into(),
        };
        assert!(should_deliver("alice", "bob", &offer));
        assert!(!should_deliver("alice", "carol", &offer));
        assert!(!should_deliver("alice", "alice", &offer));
    }

    #[test]
    fn chat_rate_limit_rejects_burst() {
        let mut limiter = RateLimiter::new();
        for _ in 0..5 {
            let msg = SignalMessage::ChatMessage {
                sender: "x".into(),
                content: "hi".into(),
                timestamp: 0,
            };
            assert!(matches!(classify(&mut limiter, &msg), Outcome::Continue));
        }
        let sixth = SignalMessage::ChatMessage {
            sender: "x".into(),
            content: "sixth".into(),
            timestamp: 0,
        };
        assert!(matches!(
            classify(&mut limiter, &sixth),
            Outcome::RateLimited(MessageKind::Chat)
        ));
    }

    #[test]
    fn signal_messages_use_the_signal_bucket() {
        let mut limiter = RateLimiter::new();
        for _ in 0..30 {
            let msg = SignalMessage::IceCandidate {
                target: "x".into(),
                sender: "y".into(),
                candidate: "c".into(),
                sdp_mid: None,
                sdp_mline_index: None,
            };
            assert!(matches!(classify(&mut limiter, &msg), Outcome::Continue));
        }
        let offer = SignalMessage::Offer {
            target: "x".into(),
            sender: "y".into(),
            sdp: "s".into(),
        };
        assert!(matches!(
            classify(&mut limiter, &offer),
            Outcome::RateLimited(MessageKind::Signal)
        ));
    }

    #[test]
    fn leave_room_is_never_rate_limited() {
        let mut limiter = RateLimiter::new();
        for _ in 0..31 {
            assert!(matches!(
                classify(&mut limiter, &SignalMessage::LeaveRoom),
                Outcome::Leave
            ));
        }
    }
}
