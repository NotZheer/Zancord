//! End-to-end WebSocket integration tests (1A.5): join/leave lifecycle,
//! directed vs broadcast routing, sender echo suppression, rate limiting,
//! malformed-message resilience, and room capacity.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use zancord_protocol::{MediaStatePayload, SignalMessage};
use zancord_signaling_server::room::RoomManager;
use zancord_signaling_server::serve;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct TestPeer {
    ws: Ws,
    id: String,
}

async fn spawn_server() -> (JoinHandle<()>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        serve(listener, Arc::new(RoomManager::new())).await.unwrap();
    });
    (handle, addr)
}

async fn connect(addr: &str, room: &str) -> Ws {
    let url = format!("ws://{addr}/ws/{room}");
    let (ws, _) = connect_async(url.as_str()).await.unwrap();
    ws
}

async fn join_peer(addr: &str, room: &str, username: &str) -> TestPeer {
    let mut ws = connect(addr, room).await;
    send(
        &mut ws,
        SignalMessage::JoinRoom {
            room_id: room.into(),
            username: username.into(),
        },
    )
    .await;
    let id = expect_room_state(&mut ws, username).await;
    TestPeer { ws, id }
}

async fn send(ws: &mut Ws, msg: SignalMessage) {
    let text = serde_json::to_string(&msg).unwrap();
    ws.send(Message::Text(text)).await.unwrap();
}

/// Reads the next text frame and parses it as a `SignalMessage`.
async fn recv(ws: &mut Ws) -> SignalMessage {
    let msg = ws
        .next()
        .await
        .expect("connection closed unexpectedly")
        .unwrap();
    match msg {
        Message::Text(t) => serde_json::from_str(&t).unwrap(),
        other => panic!("unexpected ws frame: {other:?}"),
    }
}

/// Expects `RoomState` and returns the id of the peer with `username`.
async fn expect_room_state(ws: &mut Ws, username: &str) -> String {
    match recv(ws).await {
        SignalMessage::RoomState { peers } => {
            peers
                .into_iter()
                .find(|p| p.username == username)
                .expect("self not in RoomState")
                .id
        }
        other => panic!("expected RoomState, got {other:?}"),
    }
}

/// Asserts no message arrives within 250ms (sender echo / wrong-target check).
async fn assert_silence(ws: &mut Ws) {
    let res = timeout(Duration::from_millis(250), ws.next()).await;
    assert!(
        res.is_err(),
        "expected silence, got {:?}",
        res.ok().flatten()
    );
}

#[tokio::test]
async fn three_peers_exchange_offer_answer_ice_and_chat() {
    let (handle, addr) = spawn_server().await;

    let mut alice = join_peer(&addr, "room1", "alice").await;
    let mut bob = join_peer(&addr, "room1", "bob").await;

    // alice sees bob join
    match recv(&mut alice.ws).await {
        SignalMessage::PeerJoined { peer } => assert_eq!(peer.username, "bob"),
        other => panic!("expected PeerJoined, got {other:?}"),
    }

    // carol joins; alice and bob both see it
    let mut carol = join_peer(&addr, "room1", "carol").await;
    for ws in [&mut alice.ws, &mut bob.ws] {
        match recv(ws).await {
            SignalMessage::PeerJoined { peer } => assert_eq!(peer.username, "carol"),
            other => panic!("expected PeerJoined, got {other:?}"),
        }
    }

    // offer alice -> bob reaches only bob; the server overwrites a spoofed
    // sender with the authenticated peer id
    send(
        &mut alice.ws,
        SignalMessage::Offer {
            target: bob.id.clone(),
            sender: "spoofed".into(),
            sdp: "v=0 offer".into(),
        },
    )
    .await;
    match recv(&mut bob.ws).await {
        SignalMessage::Offer {
            target,
            sender,
            sdp,
        } => {
            assert_eq!(target, bob.id);
            assert_eq!(sender, alice.id, "sender must be authenticated");
            assert_eq!(sdp, "v=0 offer");
        }
        other => panic!("expected Offer, got {other:?}"),
    }
    assert_silence(&mut carol.ws).await; // not the target
    assert_silence(&mut alice.ws).await; // no echo of own offer

    // answer bob -> alice; alice's queue proves no offer echo
    send(
        &mut bob.ws,
        SignalMessage::Answer {
            target: alice.id.clone(),
            sender: "spoofed".into(),
            sdp: "v=0 answer".into(),
        },
    )
    .await;
    match recv(&mut alice.ws).await {
        SignalMessage::Answer { target, sender, .. } => {
            assert_eq!(target, alice.id);
            assert_eq!(sender, bob.id, "sender must be authenticated");
        }
        other => panic!("expected Answer (no offer echo), got {other:?}"),
    }

    // ice candidates in both directions
    send(
        &mut bob.ws,
        SignalMessage::IceCandidate {
            target: alice.id.clone(),
            sender: "spoofed".into(),
            candidate: "cand-1".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
        },
    )
    .await;
    match recv(&mut alice.ws).await {
        SignalMessage::IceCandidate {
            candidate,
            sender,
            sdp_mid,
            sdp_mline_index,
            ..
        } => {
            assert_eq!(candidate, "cand-1");
            assert_eq!(sender, bob.id, "sender must be authenticated");
            assert_eq!(sdp_mid.as_deref(), Some("0"));
            assert_eq!(sdp_mline_index, Some(0));
        }
        other => panic!("expected IceCandidate, got {other:?}"),
    }

    send(
        &mut alice.ws,
        SignalMessage::IceCandidate {
            target: bob.id.clone(),
            sender: alice.id.clone(),
            candidate: "cand-2".into(),
            sdp_mid: None,
            sdp_mline_index: None,
        },
    )
    .await;
    match recv(&mut bob.ws).await {
        SignalMessage::IceCandidate { candidate, .. } => assert_eq!(candidate, "cand-2"),
        other => panic!("expected IceCandidate, got {other:?}"),
    }

    // chat broadcast: sender rewritten to the authenticated peer id
    send(
        &mut alice.ws,
        SignalMessage::ChatMessage {
            sender: "spoofed".into(),
            content: "hello".into(),
            timestamp: 42,
        },
    )
    .await;
    for ws in [&mut bob.ws, &mut carol.ws] {
        match recv(ws).await {
            SignalMessage::ChatMessage {
                sender,
                content,
                timestamp,
            } => {
                assert_eq!(sender, alice.id);
                assert_eq!(content, "hello");
                assert_eq!(timestamp, 42);
            }
            other => panic!("expected ChatMessage, got {other:?}"),
        }
    }
    assert_silence(&mut alice.ws).await; // no echo of own chat

    // media state broadcast; spoofed ownership is corrected to the sender id
    send(
        &mut bob.ws,
        SignalMessage::MediaState {
            peer_id: "someone-else".into(),
            state: MediaStatePayload {
                mic_enabled: true,
                ..Default::default()
            },
        },
    )
    .await;
    for ws in [&mut alice.ws, &mut carol.ws] {
        match recv(ws).await {
            SignalMessage::MediaState { peer_id, state } => {
                assert_eq!(peer_id, bob.id);
                assert!(state.mic_enabled);
            }
            other => panic!("expected MediaState, got {other:?}"),
        }
    }
    assert_silence(&mut bob.ws).await; // no echo of own media state

    // malformed JSON is ignored without killing the connection
    alice
        .ws
        .send(Message::Text("{not json".into()))
        .await
        .unwrap();
    send(
        &mut alice.ws,
        SignalMessage::ChatMessage {
            sender: alice.id.clone(),
            content: "still alive".into(),
            timestamp: 43,
        },
    )
    .await;
    match recv(&mut bob.ws).await {
        SignalMessage::ChatMessage { content, .. } => assert_eq!(content, "still alive"),
        other => panic!("expected ChatMessage after garbage, got {other:?}"),
    }

    handle.abort();
}

#[tokio::test]
async fn leave_room_broadcasts_peer_left_and_closes_connection() {
    let (handle, addr) = spawn_server().await;
    let mut alice = join_peer(&addr, "room2", "alice").await;
    let mut bob = join_peer(&addr, "room2", "bob").await;
    let _ = recv(&mut alice.ws).await; // PeerJoined(bob)

    send(&mut bob.ws, SignalMessage::LeaveRoom).await;

    match recv(&mut alice.ws).await {
        SignalMessage::PeerLeft { peer_id } => assert_eq!(peer_id, bob.id),
        other => panic!("expected PeerLeft, got {other:?}"),
    }
    // bob's socket is closed by the server: a close frame first, then stream end.
    let res = timeout(Duration::from_secs(2), bob.ws.next()).await;
    match res {
        Ok(Some(Ok(Message::Close(_)))) => {}
        other => panic!("expected close frame, got {other:?}"),
    }
    let res = timeout(Duration::from_secs(2), bob.ws.next()).await;
    assert!(
        matches!(res, Ok(None)),
        "expected bob's connection to close, got {res:?}"
    );
    handle.abort();
}

#[tokio::test]
async fn seventh_peer_is_rejected_with_room_full() {
    let (handle, addr) = spawn_server().await;
    // Keep the first six connections alive: dropping a TestPeer closes its
    // socket, which the server treats as leaving the room.
    let mut peers = Vec::new();
    for i in 0..6 {
        peers.push(join_peer(&addr, "cap", &format!("user{i}")).await);
    }
    let mut seventh = connect(&addr, "cap").await;
    send(
        &mut seventh,
        SignalMessage::JoinRoom {
            room_id: "cap".into(),
            username: "overflow".into(),
        },
    )
    .await;
    match recv(&mut seventh).await {
        SignalMessage::RoomFull => {}
        other => panic!("expected RoomFull, got {other:?}"),
    }
    handle.abort();
}

#[tokio::test]
async fn chat_rate_limit_rejects_and_does_not_relay() {
    let (handle, addr) = spawn_server().await;
    let mut alice = join_peer(&addr, "rl", "alice").await;
    let mut bob = join_peer(&addr, "rl", "bob").await;
    let _ = recv(&mut alice.ws).await; // PeerJoined(bob)

    for i in 0..5 {
        send(
            &mut alice.ws,
            SignalMessage::ChatMessage {
                sender: alice.id.clone(),
                content: format!("msg {i}"),
                timestamp: i as u64,
            },
        )
        .await;
        match recv(&mut bob.ws).await {
            SignalMessage::ChatMessage { content, .. } => assert_eq!(content, format!("msg {i}")),
            other => panic!("expected ChatMessage, got {other:?}"),
        }
    }

    // The 6th chat within a second is rejected with a rate-limit error...
    send(
        &mut alice.ws,
        SignalMessage::ChatMessage {
            sender: alice.id.clone(),
            content: "too fast".into(),
            timestamp: 99,
        },
    )
    .await;
    match recv(&mut alice.ws).await {
        SignalMessage::Error { code, .. } => assert_eq!(code, "rate_limited"),
        other => panic!("expected rate-limit Error, got {other:?}"),
    }
    // ...and never relayed to the other peer.
    assert_silence(&mut bob.ws).await;
    handle.abort();
}

#[tokio::test]
async fn invalid_room_id_is_rejected_at_http_layer() {
    let (handle, addr) = spawn_server().await;
    let url = format!("ws://{addr}/ws/bad!room");
    assert!(connect_async(url.as_str()).await.is_err());
    handle.abort();
}
