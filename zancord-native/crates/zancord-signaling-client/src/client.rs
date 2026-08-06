//! WebSocket client with exponential-backoff auto-reconnect (Phase 1B.1).
//!
//! Connects to `ws(s)://<host>:<port>/ws/<room_id>`, sends `JoinRoom`, then
//! relays parsed `SignalMessage`s over an mpsc channel. On connection loss the
//! client reconnects with exponential backoff (1s → 2s → 4s → 8s → 16s max)
//! and re-joins the room automatically.
//!
//! Outbound messages sent while disconnected are queued (bounded) and flushed
//! after the rejoin; WebRTC signaling does not survive a reconnection anyway
//! (peer ids change), so the app layer must renegotiate after a reconnect.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, warn};
use zancord_protocol::SignalMessage;

/// Outbound queue depth; `send` applies backpressure when full.
const OUTGOING_CAPACITY: usize = 64;
/// Inbound event queue depth.
const EVENTS_CAPACITY: usize = 256;
/// Reconnect backoff sequence, capped at the last value (1s→2s→4s→8s→16s).
const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// Connection lifecycle state, observable via [`SignalingClient::connection_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// First connection attempt in progress.
    Connecting,
    /// WebSocket open and room joined.
    Connected,
    /// Connection lost; reconnecting with exponential backoff.
    Reconnecting,
    /// [`SignalingClient::disconnect`] was called.
    Disconnected,
}

/// A live signaling connection.
///
/// The client owns a background task that connects, joins the room, and keeps
/// the connection alive. Dropping the client (or calling `disconnect`)
/// terminates the task and closes the event stream.
pub struct SignalingClient {
    outgoing: mpsc::Sender<SignalMessage>,
    events: Mutex<Option<mpsc::Receiver<SignalMessage>>>,
    state: watch::Sender<ConnectionState>,
    shutdown: watch::Sender<bool>,
}

impl SignalingClient {
    /// Connects to the signaling endpoint and joins `room_id` as `username`.
    ///
    /// `url` must be the full WebSocket endpoint, e.g.
    /// `ws://100.64.0.1:3000/ws/zancord-room` (or `wss://…:3443/…`).
    /// Returns immediately; observe [`Self::events`] for the first
    /// [`SignalMessage::RoomState`].
    pub async fn connect(url: &str, room_id: &str, username: &str) -> Result<Self> {
        if !(url.starts_with("ws://") || url.starts_with("wss://")) {
            return Err(anyhow!(
                "signaling url must start with ws:// or wss://, got {url}"
            ));
        }
        let (outgoing_tx, outgoing_rx) = mpsc::channel(OUTGOING_CAPACITY);
        let (events_tx, events_rx) = mpsc::channel(EVENTS_CAPACITY);
        let state_tx = watch::channel(ConnectionState::Connecting).0;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        tokio::spawn(run(
            url.to_string(),
            room_id.to_string(),
            username.to_string(),
            outgoing_rx,
            events_tx,
            state_tx.clone(),
            shutdown_rx,
        ));

        Ok(Self {
            outgoing: outgoing_tx,
            events: Mutex::new(Some(events_rx)),
            state: state_tx,
            shutdown: shutdown_tx,
        })
    }

    /// Takes the event stream. Call exactly once — the receiver cannot be
    /// cloned, matching the mpsc design.
    pub fn events(&self) -> mpsc::Receiver<SignalMessage> {
        self.events
            .lock()
            .expect("events mutex poisoned")
            .take()
            .expect("SignalingClient::events() may only be called once")
    }

    /// Sends a message to the server. Applies backpressure when the outbound
    /// queue is full; errors once the client has been disconnected.
    pub async fn send(&self, msg: SignalMessage) -> Result<()> {
        self.outgoing
            .send(msg)
            .await
            .context("signaling client is disconnected")
    }

    /// The current [`ConnectionState`].
    pub fn connection_state(&self) -> ConnectionState {
        *self.state.borrow()
    }

    /// Stops the connection task and transitions to [`ConnectionState::Disconnected`].
    pub async fn disconnect(&self) {
        let _ = self.shutdown.send(true);
        self.state.send_replace(ConnectionState::Disconnected);
    }
}

/// Connection task: connect → join → relay until the connection dies or
/// shutdown is requested; then reconnect with exponential backoff.
async fn run(
    url: String,
    room_id: String,
    username: String,
    mut outgoing: mpsc::Receiver<SignalMessage>,
    events: mpsc::Sender<SignalMessage>,
    state: watch::Sender<ConnectionState>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut backoff_index = 0usize;
    loop {
        let connected = tokio::select! {
            _ = shutdown.changed() => return,
            res = connect_async(url.as_str()) => res,
        };
        match connected {
            Ok((ws, _)) => {
                state.send_replace(ConnectionState::Connected);
                info!(target: "zancord_signaling_client", %url, "signaling connection established");
                let alive = session(
                    ws,
                    &room_id,
                    &username,
                    &mut outgoing,
                    &events,
                    &mut shutdown,
                )
                .await;
                if !alive {
                    return; // shutdown requested mid-session
                }
                backoff_index = 0;
                state.send_replace(ConnectionState::Reconnecting);
                debug!(target: "zancord_signaling_client", %url, "signaling connection lost; reconnecting");
            }
            Err(e) => {
                warn!(target: "zancord_signaling_client", %url, error = %e, "connection failed");
                state.send_replace(ConnectionState::Reconnecting);
            }
        }
        let delay = BACKOFF[backoff_index.min(BACKOFF.len() - 1)];
        backoff_index = (backoff_index + 1).min(BACKOFF.len() - 1);
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// One connected session: sends `JoinRoom`, then relays messages both ways.
/// Returns `false` when shutdown was requested, `true` when the connection
/// ended and the caller should reconnect.
async fn session(
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    room_id: &str,
    username: &str,
    outgoing: &mut mpsc::Receiver<SignalMessage>,
    events: &mpsc::Sender<SignalMessage>,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    let (mut sink, mut stream) = ws.split();
    let join = serde_json::to_string(&SignalMessage::JoinRoom {
        room_id: room_id.to_string(),
        username: username.to_string(),
    })
    .expect("SignalMessage serialization is infallible");
    if sink.send(Message::Text(join)).await.is_err() {
        return true;
    }
    info!(target: "zancord_signaling_client", room_id = %room_id, "joined room, waiting for RoomState");

    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                debug!(target: "zancord_signaling_client", "session heartbeat");
            }
            _ = shutdown.changed() => {
                debug!(target: "zancord_signaling_client", "session: shutdown requested");
                return false;
            }
            outbound = outgoing.recv() => {
                match outbound {
                    Some(msg) => {
                        let text = serde_json::to_string(&msg)
                            .expect("SignalMessage serialization is infallible");
                        if sink.send(Message::Text(text)).await.is_err() {
                            return true;
                        }
                    }
                    None => {
                        debug!(target: "zancord_signaling_client", "session: outgoing channel closed");
                        return false; // client dropped; stop the task
                    }
                }
            }
            inbound = stream.next() => {
                match inbound {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<SignalMessage>(&text) {
                            Ok(msg) => {
                                debug!(target: "zancord_signaling_client", ?msg, "received");
                                if events.send(msg).await.is_err() {
                                    debug!(target: "zancord_signaling_client", "session: events channel closed");
                                    return false; // consumer gone
                                }
                            }
                            Err(e) => warn!(target: "zancord_signaling_client", error = %e, "dropping unparseable message"),
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if sink.send(Message::Pong(payload)).await.is_err() {
                            return true;
                        }
                    }
                    Some(Ok(_)) => {} // binary / pong / close frames
                    Some(Err(e)) => {
                        debug!(target: "zancord_signaling_client", error = %e, "connection error");
                        return true;
                    }
                    None => {
                        debug!(target: "zancord_signaling_client", "session: socket closed by server");
                        return true; // server closed the connection
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zancord_protocol::{MediaStatePayload, PeerInfo};

    #[test]
    fn message_serde_roundtrip() {
        let messages = vec![
            SignalMessage::JoinRoom {
                room_id: "r".into(),
                username: "alice".into(),
            },
            SignalMessage::Offer {
                target: "peer-2".into(),
                sender: "peer-1".into(),
                sdp: "v=0\r\no=- 1 1 IN IP4 127.0.0.1".into(),
            },
            SignalMessage::IceCandidate {
                target: "peer-2".into(),
                sender: "peer-1".into(),
                candidate: "candidate:1 1 udp".into(),
                sdp_mid: None,
                sdp_mline_index: Some(0),
            },
            SignalMessage::RoomState {
                peers: vec![PeerInfo {
                    id: "p1".into(),
                    username: "alice".into(),
                    media_state: MediaStatePayload {
                        mic_enabled: true,
                        ..Default::default()
                    },
                }],
            },
            SignalMessage::Error {
                code: "rate_limited".into(),
                message: "slow down".into(),
            },
        ];
        for msg in messages {
            let json = serde_json::to_string(&msg).unwrap();
            assert_eq!(serde_json::from_str::<SignalMessage>(&json).unwrap(), msg);
        }
    }

    #[tokio::test]
    async fn rejects_non_ws_urls() {
        match SignalingClient::connect("http://example.com/ws/r", "r", "u").await {
            Err(e) => assert!(e.to_string().contains("ws://")),
            Ok(_) => panic!("non-ws url should be rejected"),
        }
    }
}
