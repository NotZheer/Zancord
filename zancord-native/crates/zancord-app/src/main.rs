//! Zancord app — Phase 2 CLI test harness (voice call over Tailscale).
//!
//! Wires the full stack without a UI:
//!
//! ```text
//! mic → cpal → HPF/gate → Opus → mesh.audio_tx → WebRTC → remote peer
//! remote peer → WebRTC → mesh incoming audio → Opus decode → mix → cpal
//! signaling client <-> mesh manager (offers/answers/ice/renegotiate)
//! ```
//!
//! Commands (stdin):
//!   mute | unmute      toggle the local mic
//!   deafen | undeafen  mute all remote audio locally
//!   vol <peer> <0..2>  per-peer volume
//!   peers              list connected peers
//!   leave              leave the room (closes the call)
//!   quit               shut everything down
//!
//! The Slint UI replaces this harness in Phase 4 (see `ui/`).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zancord_audio::pipeline::{AudioControl, AudioPipeline, PipelineConfig};
use zancord_protocol::{MediaStatePayload, PeerInfo, SignalMessage};
use zancord_signaling_client::SignalingClient;
use zancord_transport::mesh::{IceState, MeshEvent, MeshManager};

const MESH_CAPACITY: usize = 5; // 6 total including self

/// Parsed command-line invocation.
struct Args {
    ws_url: String,
    room: String,
    username: String,
    input_device: Option<String>,
    output_device: Option<String>,
}

fn usage() -> String {
    "usage: zancord-app [--list-devices] <ws-url> <room> <username> [--input <id>] [--output <id>]"
        .to_string()
}

fn parse_args() -> anyhow::Result<Option<Args>> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        std::process::exit(0);
    }
    if raw.iter().any(|a| a == "--list-devices") {
        return Ok(None);
    }
    if raw.len() < 3 {
        bail!("{}", usage());
    }
    let mut input_device = None;
    let mut output_device = None;
    let mut i = 3;
    while i < raw.len() {
        match raw[i].as_str() {
            "--input" => {
                i += 1;
                input_device = Some(raw.get(i).context("--input requires a device id")?.clone());
            }
            "--output" => {
                i += 1;
                output_device = Some(raw.get(i).context("--output requires a device id")?.clone());
            }
            other => bail!("unknown argument: {other}\n{}", usage()),
        }
        i += 1;
    }
    Ok(Some(Args {
        ws_url: raw[0].clone(),
        room: raw[1].clone(),
        username: raw[2].clone(),
        input_device,
        output_device,
    }))
}

/// Returns the host default device id, or the first device, or `None`.
fn default_device(devices: Vec<zancord_audio::devices::AudioDevice>) -> Option<String> {
    devices
        .iter()
        .find(|d| d.is_default)
        .or_else(|| devices.first())
        .map(|d| d.id.clone())
}

fn print_devices() -> anyhow::Result<()> {
    println!("Input devices:");
    for d in zancord_audio::devices::list_input_devices()? {
        println!(
            "  {:<40} {}",
            d.id,
            if d.is_default { "(default)" } else { "" }
        );
    }
    println!("Output devices:");
    for d in zancord_audio::devices::list_output_devices()? {
        println!(
            "  {:<40} {}",
            d.id,
            if d.is_default { "(default)" } else { "" }
        );
    }
    Ok(())
}

/// Waits for the first `RoomState`, which the server sends immediately after
/// join. Returns our server-assigned peer id and the peers already present.
async fn wait_for_room_state(
    events: &mut mpsc::Receiver<SignalMessage>,
    username: &str,
) -> anyhow::Result<(String, Vec<PeerInfo>)> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .context("no RoomState from the signaling server within 10s")?
            .context("signaling connection closed before RoomState")?;
        match msg {
            SignalMessage::RoomState { peers } => {
                let self_id = peers
                    .iter()
                    .find(|p| p.username == username)
                    .map(|p| p.id.clone())
                    .context("own username not present in RoomState")?;
                return Ok((self_id, peers));
            }
            SignalMessage::RoomFull => bail!("room is full"),
            SignalMessage::Error { code, message } => bail!("server error: {code}: {message}"),
            other => debug!(?other, "ignoring message before RoomState"),
        }
    }
}

/// Rebuilds the mesh with a fresh server-assigned id (signaling reconnect
/// assigns a new id; the old mesh's peer connections are stale). Returns the
/// mesh and its event receiver — the receiver is subscribed BEFORE the peers
/// join so no `PeerConnected` is lost (a lost event silences that peer).
async fn rebuild_mesh(
    old: Option<MeshManager>,
    new_id: &str,
    peers: &[PeerInfo],
    mesh_sig_tx: mpsc::Sender<SignalMessage>,
) -> anyhow::Result<(MeshManager, tokio::sync::broadcast::Receiver<MeshEvent>)> {
    if let Some(mut mesh) = old {
        if let Err(err) = mesh.shutdown().await {
            warn!(error = %err, "old mesh shutdown failed");
        }
    }
    let mut mesh = MeshManager::new(new_id.to_owned(), mesh_sig_tx, MESH_CAPACITY)?;
    let events = mesh.event_rx();
    for peer in peers.iter().filter(|p| p.id != new_id) {
        if let Err(err) = mesh.handle_peer_joined(peer.clone()).await {
            warn!(peer = %peer.id, error = %err, "join failed for existing peer");
        }
    }
    Ok((mesh, events))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,zancord_transport=debug,zancord_audio=info".into()),
        )
        .init();

    let Some(args) = parse_args()? else {
        print_devices()?;
        return Ok(());
    };

    // Resolve default devices when not overridden (`None` would disable the
    // direction entirely, which is only desired for the explicit test mode).
    let input_device = match args.input_device.clone() {
        Some(id) => Some(id),
        None => default_device(zancord_audio::devices::list_input_devices()?),
    };
    let output_device = match args.output_device.clone() {
        Some(id) => Some(id),
        None => default_device(zancord_audio::devices::list_output_devices()?),
    };
    if input_device.is_none() {
        warn!("no input device found; mic capture disabled");
    }
    if output_device.is_none() {
        warn!("no output device found; playback disabled");
    }

    // The client expects the full endpoint URL; accept either a base URL
    // (`ws://host:port`) or the full path (`ws://host:port/ws/<room>`).
    let endpoint = if args.ws_url.contains("/ws/") {
        args.ws_url.clone()
    } else {
        format!("{}/ws/{}", args.ws_url.trim_end_matches('/'), args.room)
    };
    let client = Arc::new(
        SignalingClient::connect(&endpoint, &args.room, &args.username)
            .await
            .context("failed to start signaling client")?,
    );
    let mut events = client.events();

    let (self_id, initial_peers) = wait_for_room_state(&mut events, &args.username).await?;
    info!(self_id = %self_id, peers = initial_peers.len(), "joined room {}", args.room);

    // Mesh: outbound signaling flows into this channel and out via the client.
    let (mesh_sig_tx, mut mesh_sig_rx) = mpsc::channel(256);
    let mut mesh: Option<MeshManager> = Some(
        MeshManager::new(self_id.clone(), mesh_sig_tx.clone(), MESH_CAPACITY)
            .context("mesh creation failed")?,
    );
    // Subscribe BEFORE joining the initial peers: `PeerConnected` is broadcast
    // during `handle_peer_joined`, and a lost event means the incoming audio
    // channel for that peer is never claimed — silent one-way audio.
    let mut mesh_events = mesh.as_ref().expect("mesh exists").event_rx();
    for peer in initial_peers.iter().filter(|p| p.id != self_id) {
        mesh.as_mut()
            .expect("mesh exists")
            .handle_peer_joined(peer.clone())
            .await?;
    }

    // Audio: encoded frames out via the mesh, remote frames in via a tagged
    // channel fed by per-peer forwarders.
    let (audio_in_tx, audio_in_rx) = mpsc::channel(256);
    let (control_tx, control_rx) = mpsc::channel(64);
    let audio_handle = AudioPipeline::spawn(
        PipelineConfig::default(),
        input_device.clone(),
        output_device.clone(),
        mesh.as_ref().expect("mesh exists").audio_tx(),
        audio_in_rx,
        control_rx,
    )
    .context("audio pipeline failed to start")?;
    info!("audio pipeline running (mic -> Opus -> mesh, mesh -> mix -> speakers)");

    let mut local_state = MediaStatePayload {
        mic_enabled: true,
        ..Default::default()
    };

    // Interactive commands from stdin.
    let (cmd_tx, mut cmd_rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if cmd_tx.send(line.trim().to_string()).await.is_err() {
                return;
            }
        }
    });

    loop {
        tokio::select! {
            msg = events.recv() => {
                let Some(msg) = msg else {
                    warn!("signaling event stream closed; exiting");
                    break;
                };

                // RoomState needs the outer `mesh` (a rebuild replaces it).
                if let SignalMessage::RoomState { peers } = &msg {
                    let Some(new_self) = peers.iter().find(|p| p.username == args.username) else {
                        continue;
                    };
                    let current = mesh.as_ref().map(|m| m.local_id().to_string());
                    if current.as_deref() != Some(new_self.id.as_str()) {
                        info!(new_id = %new_self.id, "reconnected with a new peer id; rebuilding mesh");
                        let old = mesh.take();
                        let (new_mesh, new_events) =
                            rebuild_mesh(old, &new_self.id, peers, mesh_sig_tx.clone()).await?;
                        mesh = Some(new_mesh);
                        mesh_events = new_events;
                    } else {
                        let mesh_ref = mesh.as_mut().expect("mesh exists");
                        let local_id = mesh_ref.local_id().to_string();
                        for peer in peers.iter().filter(|p| p.id != local_id) {
                            mesh_ref.handle_peer_joined(peer.clone()).await?;
                        }
                    }
                    continue;
                }

                let Some(mesh) = mesh.as_mut() else { continue };
                match msg {
                    SignalMessage::PeerJoined { peer } => {
                        if peer.id == mesh.local_id() { continue; }
                        info!(peer = %peer.id, username = %peer.username, "peer joined");
                        if let Err(err) = mesh.handle_peer_joined(peer).await {
                            warn!(error = %err, "peer join failed");
                        }
                    }
                    SignalMessage::PeerLeft { peer_id } => {
                        info!(peer = %peer_id, "peer left");
                        if let Err(err) = mesh.handle_peer_left(&peer_id).await {
                            warn!(error = %err, "peer leave failed");
                        }
                        let _ = control_tx.send(AudioControl::RemovePeer { peer: peer_id }).await;
                    }
                    SignalMessage::Offer { .. }
                    | SignalMessage::Answer { .. }
                    | SignalMessage::IceCandidate { .. }
                    | SignalMessage::Renegotiate { .. } => {
                        if let Err(err) = mesh.handle_signal(msg).await {
                            warn!(error = %err, "signal handling failed");
                        }
                    }
                    SignalMessage::MediaState { ref peer_id, .. } => {
                        if *peer_id != mesh.local_id() {
                            if let Err(err) = mesh.handle_signal(msg).await {
                                warn!(error = %err, "media state handling failed");
                            }
                        }
                    }
                    SignalMessage::ChatMessage { sender, content, .. } => {
                        if sender != mesh.local_id() {
                            println!("[chat] {sender}: {content}");
                        }
                    }
                    SignalMessage::RoomFull => {
                        warn!("room is full");
                        break;
                    }
                    SignalMessage::Error { code, message } => {
                        warn!(code = %code, "server error: {message}");
                    }
                    _ => {}
                }
            }
            mev = mesh_events.recv() => {
                let Some(mev) = mev.ok() else { continue };
                match mev {
                    MeshEvent::PeerConnected { peer_id } => {
                        let Some(mesh) = mesh.as_mut() else { continue };
                        if let Some(mut rx) = mesh.take_incoming_audio(&peer_id) {
                            info!(peer = %peer_id, "claimed incoming audio channel");
                            let tagged = audio_in_tx.clone();
                            let pid = peer_id.clone();
                            tokio::spawn(async move {
                                let mut first = true;
                                while let Some(frame) = rx.recv().await {
                                    if first {
                                        info!(peer = %pid, "first remote audio frame forwarded");
                                        first = false;
                                    }
                                    if tagged.send((pid.clone(), frame)).await.is_err() {
                                        return;
                                    }
                                }
                            });
                        } else {
                            warn!(peer = %peer_id, "incoming audio channel already claimed or missing");
                        }
                    }
                    MeshEvent::PeerDisconnected { peer_id } => {
                        let _ = control_tx.send(AudioControl::RemovePeer { peer: peer_id }).await;
                    }
                    MeshEvent::IceStateChanged { peer_id, state } => {
                        let label = match state {
                            IceState::Connected | IceState::Completed => "connected",
                            IceState::Failed => "failed",
                            IceState::Disconnected => "disconnected",
                            _ => "connecting",
                        };
                        info!(peer = %peer_id, %label, "ice state");
                    }
                    MeshEvent::MediaState { peer_id, state } => {
                        info!(peer = %peer_id, mic = state.mic_enabled, screen = state.screen_sharing, "peer media state");
                    }
                }
            }
            msg = mesh_sig_rx.recv() => {
                let Some(msg) = msg else { continue };
                if let Err(err) = client.send(msg).await {
                    warn!(error = %err, "signaling send failed");
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd.as_str() {
                    "mute" => {
                        local_state.mic_enabled = false;
                        let _ = client.send(SignalMessage::MediaState { peer_id: mesh.as_ref().expect("mesh").local_id().to_string(), state: local_state }).await;
                        println!("mic muted");
                    }
                    "unmute" => {
                        local_state.mic_enabled = true;
                        let _ = client.send(SignalMessage::MediaState { peer_id: mesh.as_ref().expect("mesh").local_id().to_string(), state: local_state }).await;
                        println!("mic unmuted");
                    }
                    "deafen" => {
                        local_state.deafened = true;
                        let _ = control_tx.send(AudioControl::SetDeafened { deafened: true }).await;
                        let _ = client.send(SignalMessage::MediaState { peer_id: mesh.as_ref().expect("mesh").local_id().to_string(), state: local_state }).await;
                        println!("deafened");
                    }
                    "undeafen" => {
                        local_state.deafened = false;
                        let _ = control_tx.send(AudioControl::SetDeafened { deafened: false }).await;
                        let _ = client.send(SignalMessage::MediaState { peer_id: mesh.as_ref().expect("mesh").local_id().to_string(), state: local_state }).await;
                        println!("undeafened");
                    }
                    _ if cmd.starts_with("vol ") => {
                        let parts: Vec<&str> = cmd.split_whitespace().collect();
                        if parts.len() == 3 {
                            let volume: f32 = match parts[2].parse() {
                                Ok(v) => v,
                                Err(_) => { println!("volume must be 0.0..2.0"); continue; }
                            };
                            let _ = control_tx.send(AudioControl::SetPeerVolume { peer: parts[1].to_string(), volume }).await;
                            println!("volume for {} set to {volume}", parts[1]);
                        } else {
                            println!("usage: vol <peer-id> <0.0..2.0>");
                        }
                    }
                    "peers" => {
                        let Some(mesh) = mesh.as_ref() else { continue };
                        let ids = mesh.peer_ids();
                        println!("connected peers ({}): {}", ids.len(), ids.join(", "));
                    }
                    "leave" => {
                        let _ = client.send(SignalMessage::LeaveRoom).await;
                        client.disconnect().await;
                        println!("left room");
                        break;
                    }
                    "quit" => {
                        let _ = control_tx.send(AudioControl::Shutdown).await;
                        let _ = client.send(SignalMessage::LeaveRoom).await;
                        client.disconnect().await;
                        break;
                    }
                    other => println!("unknown command: {other} (mute|unmute|deafen|undeafen|vol <peer> <0..2>|peers|leave|quit)"),
                }
            }
        }
    }

    if let Some(mesh) = mesh.as_mut() {
        if let Err(err) = mesh.shutdown().await {
            warn!(error = %err, "mesh shutdown failed");
        }
    }
    // Stop the audio pipeline (the CLI exits after leave/quit; a future UI
    // would keep the pipeline across room changes).
    let _ = control_tx.send(AudioControl::Shutdown).await;
    if audio_handle.join().is_err() {
        warn!("audio thread panicked");
    }
    info!("zancord-app exiting");
    Ok(())
}
