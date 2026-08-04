//! Integration tests against a real signaling server (1B.3): join/room-state,
//! message relay, and reconnection with backoff + rejoin after a restart.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use zancord_protocol::SignalMessage;
use zancord_signaling_client::{ConnectionState, SignalingClient};
use zancord_signaling_server::room::RoomManager;
use zancord_signaling_server::serve;

const ROOM: &str = "test-room";

async fn spawn_server() -> (tokio::task::JoinHandle<()>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        serve(listener, Arc::new(RoomManager::new())).await.unwrap();
    });
    (handle, addr)
}

async fn next_message(rx: &mut mpsc::Receiver<SignalMessage>) -> SignalMessage {
    timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for message")
        .expect("event stream closed")
}

async fn wait_for_state(client: &SignalingClient, expected: ConnectionState, max: Duration) {
    let deadline = tokio::time::Instant::now() + max;
    loop {
        if client.connection_state() == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "client never reached {expected:?}"
        );
        sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn connects_and_receives_room_state() {
    let (handle, addr) = spawn_server().await;
    let url = format!("ws://{addr}/ws/{ROOM}");
    let client = SignalingClient::connect(&url, ROOM, "alice").await.unwrap();
    let mut events = client.events();

    match next_message(&mut events).await {
        SignalMessage::RoomState { peers } => {
            assert_eq!(peers.len(), 1);
            assert_eq!(peers[0].username, "alice");
        }
        other => panic!("expected RoomState, got {other:?}"),
    }
    assert_eq!(client.connection_state(), ConnectionState::Connected);

    client.disconnect().await;
    handle.abort();
}

#[tokio::test]
async fn two_clients_relay_chat() {
    let (handle, addr) = spawn_server().await;
    let url = format!("ws://{addr}/ws/{ROOM}");

    let alice = SignalingClient::connect(&url, ROOM, "alice").await.unwrap();
    let mut alice_events = alice.events();
    let _ = next_message(&mut alice_events).await; // RoomState

    let bob = SignalingClient::connect(&url, ROOM, "bob").await.unwrap();
    let mut bob_events = bob.events();
    let _ = next_message(&mut bob_events).await; // RoomState (alice + bob)

    match next_message(&mut alice_events).await {
        SignalMessage::PeerJoined { peer } => assert_eq!(peer.username, "bob"),
        other => panic!("expected PeerJoined, got {other:?}"),
    }

    alice
        .send(SignalMessage::ChatMessage {
            sender: "alice".into(),
            content: "hi bob".into(),
            timestamp: 1,
        })
        .await
        .unwrap();
    match next_message(&mut bob_events).await {
        SignalMessage::ChatMessage {
            sender, content, ..
        } => {
            assert_eq!(content, "hi bob");
            assert_ne!(sender, "alice"); // server rewrites sender to the peer id
        }
        other => panic!("expected ChatMessage, got {other:?}"),
    }

    alice.disconnect().await;
    bob.disconnect().await;
    handle.abort();
}

#[tokio::test]
async fn reconnects_with_backoff_and_rejoins_after_server_restart() {
    // Server #1 runs on its own runtime/thread so it can be killed hard
    // (in-flight connections included) and restarted on the same port.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            tokio::select! {
                _ = serve(listener, Arc::new(RoomManager::new())) => {}
                _ = shutdown_rx => {}
            }
        });
        rt.shutdown_timeout(Duration::ZERO); // drop all connection tasks
    });

    let url = format!("ws://{addr}/ws/{ROOM}");
    let client = SignalingClient::connect(&url, ROOM, "carol").await.unwrap();
    let mut events = client.events();
    let _ = next_message(&mut events).await; // RoomState from server #1
    assert_eq!(client.connection_state(), ConnectionState::Connected);

    // Kill server #1; the client notices the dead connection.
    shutdown_tx.send(()).unwrap();
    thread.join().unwrap();
    wait_for_state(
        &client,
        ConnectionState::Reconnecting,
        Duration::from_secs(5),
    )
    .await;

    // Server #2 on the same port.
    let listener2 = TcpListener::bind(addr).await.unwrap();
    let handle2 = tokio::spawn(async move {
        serve(listener2, Arc::new(RoomManager::new()))
            .await
            .unwrap();
    });

    // Client reconnects (1s backoff), rejoins, and receives a fresh RoomState.
    wait_for_state(&client, ConnectionState::Connected, Duration::from_secs(10)).await;
    match next_message(&mut events).await {
        SignalMessage::RoomState { peers } => assert_eq!(peers.len(), 1),
        other => panic!("expected RoomState after rejoin, got {other:?}"),
    }

    client.disconnect().await;
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    handle2.abort();
}
