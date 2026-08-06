//! App orchestrator (Phase 4.10): central coordinator wiring the Slint main
//! window to the signaling client, mesh, and audio pipeline.
//!
//! Threading: this module runs on a tokio worker thread. Every UI mutation
//! goes through `Weak<MainWindow>::upgrade_in_event_loop`, which runs on the
//! UI thread (models are `VecModel`s mutated by downcast there). UI callbacks
//! push intents into a command channel processed here.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};
use tokio::sync::{broadcast, mpsc};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::{ChatMessage, MainWindow, PeerData, Toast};
use zancord_audio::pipeline::{AudioControl, AudioPipeline, PipelineConfig};
use zancord_audio::IncomingAudioKind;
use zancord_protocol::{EncodedAudioFrame, MediaStatePayload, PeerInfo, SignalMessage};
use zancord_signaling_client::SignalingClient;
use zancord_transport::mesh::{IceState, MeshEvent, MeshManager};

/// Mesh capacity: 5 remote peers (6 total including self).
const MESH_CAPACITY: usize = 5;

/// Intents from the UI, processed by the orchestrator's event loop.
enum UiCommand {
    ToggleMic,
    ToggleCamera,
    ToggleScreenShare,
    ToggleDeafen,
    SendChat(String),
    Leave,
    CopyLink,
    ToggleChat,
    SelectCamera(u32),
}

pub struct App {
    window: Weak<MainWindow>,
    client: Arc<SignalingClient>,
    room: String,
    endpoint: String,
    self_id: String,
    /// peer id → username (for tiles; the mesh only tracks ids).
    usernames: HashMap<String, String>,
    mesh: Option<MeshManager>,
    mesh_events: Option<broadcast::Receiver<MeshEvent>>,
    audio_control: Option<mpsc::Sender<AudioControl>>,
    screen_share: Option<crate::screen_share::ScreenShareSession>,
    camera_session: Option<crate::camera::CameraSession>,
    local_state: MediaStatePayload,
    /// Enumerated cameras (UI picker) + the current selection.
    cameras: Vec<zancord_capture::CameraSource>,
    camera_index: u32,
    cmd_tx: mpsc::Sender<UiCommand>,
}

impl App {
    /// Connects everything and runs the event loop until the window closes or
    /// the user leaves the room.
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        window: Weak<MainWindow>,
        ws_url: String,
        room: String,
        username: String,
        input_device: Option<String>,
        output_device: Option<String>,
    ) -> anyhow::Result<()> {
        let endpoint = if ws_url.contains("/ws/") {
            ws_url
        } else {
            format!("{}/ws/{}", ws_url.trim_end_matches('/'), room)
        };
        let client = Arc::new(
            SignalingClient::connect(&endpoint, &room, &username)
                .await
                .context("failed to start signaling client")?,
        );
        let mut events = client.events();

        // The server sends RoomState immediately after join.
        let (self_id, initial_peers) = wait_for_room_state(&mut events, &username).await?;
        info!(self_id = %self_id, peers = initial_peers.len(), "joined room {room}");

        // Mesh: outbound signaling flows through this channel to the client.
        let (mesh_sig_tx, mut mesh_sig_rx) = mpsc::channel(256);
        let mut mesh = MeshManager::new(self_id.clone(), mesh_sig_tx.clone(), MESH_CAPACITY)
            .context("mesh creation failed")?;
        // Subscribe BEFORE joining initial peers: `PeerConnected` is broadcast
        // during `handle_peer_joined` (see the one-way-audio fix).
        let mesh_events = mesh.event_rx();
        let usernames: HashMap<String, String> = initial_peers
            .iter()
            .filter(|p| p.id != self_id)
            .map(|p| (p.id.clone(), p.username.clone()))
            .collect();
        for peer in initial_peers.iter().filter(|p| p.id != self_id) {
            mesh.handle_peer_joined(peer.clone()).await?;
        }

        // Audio: encoded frames out via the mesh, remote frames in via tagged
        // channels fed by per-peer forwarders.
        let (audio_in_tx, audio_in_rx) = mpsc::channel(256);
        let (control_tx, control_rx) = mpsc::channel(64);
        let audio_handle = AudioPipeline::spawn(
            PipelineConfig::default(),
            input_device,
            output_device,
            mesh.audio_tx(),
            audio_in_rx,
            control_rx,
        )
        .context("audio pipeline failed to start")?;
        info!("audio pipeline running (mic -> Opus -> mesh, mesh -> mix -> speakers)");

        // Camera picker state: enumerate devices once; prefer the saved
        // choice when it still exists.
        let cameras = zancord_capture::available_cameras();
        let config = crate::config::AppConfig::load();
        let camera_index = config
            .camera_index
            .filter(|i| cameras.iter().any(|c| c.index == *i))
            .unwrap_or(0);

        let (cmd_tx, mut cmd_rx) = mpsc::channel(64);
        let mut app = Self {
            window: window.clone(),
            client,
            room: room.to_owned(),
            endpoint,
            self_id,
            usernames,
            mesh: Some(mesh),
            mesh_events: Some(mesh_events),
            audio_control: Some(control_tx),
            screen_share: None,
            camera_session: None,
            local_state: MediaStatePayload {
                mic_enabled: true,
                ..Default::default()
            },
            cameras,
            camera_index,
            cmd_tx,
        };

        app.init_ui();
        info!("ui initialized; wiring callbacks");
        app.wire_callbacks()?;
        info!("callbacks wired");
        app.notify("Connected to room".into(), "success");
        info!("entering event loop");

        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    debug!(target: "zancord_app", "app heartbeat");
                }
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        warn!("command channel closed; exiting");
                        break;
                    };
                    if !app.handle_command(cmd).await? {
                        info!("command requested exit");
                        break;
                    }
                }
                msg = events.recv() => {
                    let Some(msg) = msg else {
                        warn!("signaling event stream closed; exiting");
                        break;
                    };
                    if !app.handle_signal(msg).await? {
                        info!("signal requested exit");
                        break; // room full / fatal
                    }
                }
                mev = app.mesh_events.as_mut().expect("mesh events").recv() => {
                    let Ok(mev) = mev else { continue };
                    app.handle_mesh_event(mev, &audio_in_tx).await;
                }
                sig = mesh_sig_rx.recv() => {
                    let Some(sig) = sig else { continue };
                    if let Err(err) = app.client.send(sig).await {
                        warn!(error = %err, "signaling send failed");
                    }
                }
            }
        }

        // Cleanup.
        if let Some(mut mesh) = app.mesh.take() {
            if let Err(err) = mesh.shutdown().await {
                warn!(error = %err, "mesh shutdown failed");
            }
        }
        if let Some(tx) = app.audio_control.take() {
            let _ = tx.try_send(AudioControl::Shutdown);
        }
        if audio_handle.join().is_err() {
            warn!("audio thread panicked");
        }
        app.client.disconnect().await;
        info!("zancord-ui exiting");
        Ok(())
    }

    fn init_ui(&self) {
        let window = self.window.clone();
        let room = self.room.clone();
        let camera_names: Vec<slint::SharedString> = self
            .cameras
            .iter()
            .map(|c| c.name.as_str().into())
            .collect();
        let current_camera = self
            .cameras
            .iter()
            .find(|c| c.index == self.camera_index)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| format!("Camera {}", self.camera_index));
        let _ = window.upgrade_in_event_loop(move |w| {
            w.set_room_name(room.into());
            // Seed the models with empty VecModels so later mutations can
            // downcast `ModelRc` to `VecModel` on the UI thread.
            w.set_peers(ModelRc::new(VecModel::default()));
            w.set_chat_messages(ModelRc::new(VecModel::default()));
            w.set_toasts(ModelRc::new(VecModel::default()));
            w.set_cameras(ModelRc::new(VecModel::from(camera_names)));
            w.set_current_camera_name(current_camera.into());

            w.set_peer_count(0);
            w.set_mic_enabled(true);
            w.set_camera_enabled(false);
            w.set_screen_sharing(false);
            w.set_deafened(false);
            w.set_local_has_video(false);
        });
    }

    fn wire_callbacks(&self) -> anyhow::Result<()> {
        // Slint's `Weak::upgrade()` only succeeds on the UI thread — component
        // mutation (including callback registration) must happen there, so do
        // it inside `upgrade_in_event_loop`.
        let window = self.window.clone();
        let cmd = self.cmd_tx.clone();
        window
            .upgrade_in_event_loop(move |w| {
                let base = cmd;
                // UI callbacks run on the UI thread, which sits inside tokio's
                // block_on context — `blocking_send` would panic there, so use
                // `try_send` (channel capacity 64; a dropped click is fine).
                let c = base.clone();
                w.on_toggle_mic(move || {
                    let _ = c.try_send(UiCommand::ToggleMic);
                });
                let c = base.clone();
                w.on_toggle_camera(move || {
                    let _ = c.try_send(UiCommand::ToggleCamera);
                });
                let c = base.clone();
                w.on_toggle_screen_share(move || {
                    let _ = c.try_send(UiCommand::ToggleScreenShare);
                });
                let c = base.clone();
                w.on_toggle_deafen(move || {
                    let _ = c.try_send(UiCommand::ToggleDeafen);
                });
                let c = base.clone();
                w.on_leave_room(move || {
                    let _ = c.try_send(UiCommand::Leave);
                });
                let c = base.clone();
                w.on_send_chat(move |content| {
                    let _ = c.try_send(UiCommand::SendChat(content.to_string()));
                });
                let c = base.clone();
                w.on_copy_invite_link(move || {
                    let _ = c.try_send(UiCommand::CopyLink);
                });
                let c = base.clone();
                w.on_toggle_chat(move || {
                    let _ = c.try_send(UiCommand::ToggleChat);
                });
                let c = base.clone();
                w.on_camera_selected(move |index| {
                    let _ = c.try_send(UiCommand::SelectCamera(index as u32));
                });
            })
            .map_err(|_| anyhow::anyhow!("window dropped before callbacks wired"))?;
        Ok(())
    }

    /// Runs one UI intent; returns false when the loop must end.
    async fn handle_command(&mut self, cmd: UiCommand) -> anyhow::Result<bool> {
        let Some(mesh) = self.mesh.as_mut() else {
            return Ok(true);
        };
        let local_id = self.self_id.clone();
        match cmd {
            UiCommand::ToggleMic => {
                self.local_state.mic_enabled = !self.local_state.mic_enabled;
                let _ = self
                    .client
                    .send(SignalMessage::MediaState {
                        peer_id: local_id,
                        state: self.local_state,
                    })
                    .await;
                let on = self.local_state.mic_enabled;
                let window = self.window.clone();
                let _ = window.upgrade_in_event_loop(move |w| w.set_mic_enabled(on));
            }
            UiCommand::ToggleCamera => {
                let on = !self.local_state.camera_enabled;
                self.local_state.camera_enabled = on;
                mesh.set_camera_enabled(on).await?;
                let _ = self
                    .client
                    .send(SignalMessage::MediaState {
                        peer_id: local_id.clone(),
                        state: self.local_state,
                    })
                    .await;
                if on {
                    if !self.start_camera_session(self.camera_index).await? {
                        return Ok(true); // failure already reverted + toasted
                    }
                } else if let Some(mut session) = self.camera_session.take() {
                    session.stop();
                    let window = self.window.clone();
                    let _ = window.upgrade_in_event_loop(|w| {
                        w.set_local_has_video(false);
                    });
                }
                let window = self.window.clone();
                let _ = window.upgrade_in_event_loop(move |w| {
                    w.set_camera_enabled(on);
                    w.set_local_has_video(on);
                });
                self.notify(
                    if on {
                        "Camera started".into()
                    } else {
                        "Camera stopped".into()
                    },
                    "info",
                );
            }
            UiCommand::SelectCamera(index) => {
                self.camera_index = index;
                let mut config = crate::config::AppConfig::load();
                config.camera_index = Some(index);
                if let Err(err) = config.save() {
                    warn!(error = %err, "failed to save camera preference");
                }
                if self.local_state.camera_enabled {
                    // Restart capture on the new device: drop the old session
                    // and track, then re-add + reopen (the camera flag itself
                    // is unchanged, so no MediaState broadcast is needed).
                    if let Some(mut session) = self.camera_session.take() {
                        session.stop();
                    }
                    mesh.set_camera_enabled(false).await?;
                    mesh.set_camera_enabled(true).await?;
                    if !self.start_camera_session(index).await? {
                        return Ok(true);
                    }
                }
                let name = self
                    .cameras
                    .iter()
                    .find(|c| c.index == index)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| format!("Camera {index}"));
                let window = self.window.clone();
                let _ = window.upgrade_in_event_loop(move |w| {
                    w.set_current_camera_name(name.into());
                });
            }
            UiCommand::ToggleScreenShare => {
                let on = !self.local_state.screen_sharing;
                self.local_state.screen_sharing = on;
                mesh.set_screen_enabled(on).await?;
                let _ = self
                    .client
                    .send(SignalMessage::MediaState {
                        peer_id: local_id.clone(),
                        state: self.local_state,
                    })
                    .await;
                if on {
                    let screen_tx = mesh.screen_tx();
                    let screen_audio_tx = mesh.screen_audio_tx();
                    let feedback_rx = mesh.feedback_rx();
                    let window_clone = self.window.clone();
                    // Platform capture (portal picker / SCK) may block for
                    // seconds — never run it on the async UI context.
                    let start_res = tokio::task::spawn_blocking(move || {
                        crate::screen_share::ScreenShareSession::start_with_channels(
                            screen_tx,
                            screen_audio_tx,
                            feedback_rx,
                            window_clone,
                        )
                    })
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking failed: {e}")));
                    match start_res {
                        Ok(session) => self.screen_share = Some(session),
                        Err(err) => {
                            self.local_state.screen_sharing = false;
                            mesh.set_screen_enabled(false).await?;
                            let _ = self
                                .client
                                .send(SignalMessage::MediaState {
                                    peer_id: local_id.clone(),
                                    state: self.local_state,
                                })
                                .await;
                            self.notify(format!("Screen share failed: {err}"), "error");
                            let window = self.window.clone();
                            let _ = window.upgrade_in_event_loop(|w| w.set_screen_sharing(false));
                            return Ok(true);
                        }
                    }
                } else if let Some(mut session) = self.screen_share.take() {
                    session.stop();
                    let window = self.window.clone();
                    let _ = window.upgrade_in_event_loop(|w| {
                        w.set_local_has_video(false);
                    });
                }
                let window = self.window.clone();
                let _ = window.upgrade_in_event_loop(move |w| w.set_screen_sharing(on));
                self.notify(
                    if on {
                        "Screen share started".into()
                    } else {
                        "Screen share stopped".into()
                    },
                    "info",
                );
            }
            UiCommand::ToggleDeafen => {
                self.local_state.deafened = !self.local_state.deafened;
                if let Some(tx) = &self.audio_control {
                    let _ = tx
                        .send(AudioControl::SetDeafened {
                            deafened: self.local_state.deafened,
                        })
                        .await;
                }
                let _ = self
                    .client
                    .send(SignalMessage::MediaState {
                        peer_id: local_id,
                        state: self.local_state,
                    })
                    .await;
                let on = self.local_state.deafened;
                let window = self.window.clone();
                let _ = window.upgrade_in_event_loop(move |w| w.set_deafened(on));
            }
            UiCommand::SendChat(content) => {
                self.append_chat("You".to_owned(), content.clone(), true);
                if let Err(err) = self
                    .client
                    .send(SignalMessage::ChatMessage {
                        sender: local_id,
                        content,
                        timestamp: now_ms(),
                    })
                    .await
                {
                    warn!(error = %err, "chat send failed");
                }
            }
            UiCommand::Leave => {
                let _ = self.client.send(SignalMessage::LeaveRoom).await;
                self.client.disconnect().await;
                info!("left room");
                // Close the window and end the loop.
                let window = self.window.clone();
                let _ = window.upgrade_in_event_loop(|w| {
                    let _ = w.hide();
                });
                return Ok(false);
            }
            UiCommand::CopyLink => {
                // The signaling endpoint doubles as the invite link.
                let endpoint = self.endpoint.clone();
                let copied = arboard::Clipboard::new()
                    .and_then(|mut c| c.set_text(endpoint.clone()))
                    .is_ok();
                self.notify(
                    if copied {
                        "Invite link copied".into()
                    } else {
                        format!("Invite link: {endpoint}")
                    },
                    if copied { "success" } else { "info" },
                );
            }
            UiCommand::ToggleChat => {
                let window = self.window.clone();
                let _ = window.upgrade_in_event_loop(|w| {
                    w.set_chat_visible(!w.get_chat_visible());
                });
            }
        }
        Ok(true)
    }

    /// Starts the capture session for `camera_index`. On failure the mesh
    /// track + media state are reverted and an error toast is shown; returns
    /// `false` in that case.
    async fn start_camera_session(&mut self, index: u32) -> anyhow::Result<bool> {
        let Some(mesh) = self.mesh.as_mut() else {
            return Ok(false);
        };
        let camera_tx = mesh.camera_tx();
        let feedback_rx = mesh.feedback_rx();
        let window_clone = self.window.clone();
        // Opening the device (AVFoundation handshake) may block for seconds —
        // never run it on the async UI context.
        let start_res = tokio::task::spawn_blocking(move || {
            crate::camera::CameraSession::start_with_channels(
                camera_tx,
                feedback_rx,
                window_clone,
                index,
            )
        })
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking failed: {e}")));
        match start_res {
            Ok(session) => {
                self.camera_session = Some(session);
                Ok(true)
            }
            Err(err) => {
                self.local_state.camera_enabled = false;
                if let Some(mesh) = self.mesh.as_mut() {
                    mesh.set_camera_enabled(false).await?;
                }
                let _ = self
                    .client
                    .send(SignalMessage::MediaState {
                        peer_id: self.self_id.clone(),
                        state: self.local_state,
                    })
                    .await;
                self.notify(format!("Camera failed: {err}"), "error");
                let window = self.window.clone();
                let _ = window.upgrade_in_event_loop(|w| {
                    w.set_camera_enabled(false);
                    w.set_local_has_video(false);
                });
                Ok(false)
            }
        }
    }

    /// Routes one signaling message; returns false when the loop must end.
    async fn handle_signal(&mut self, msg: SignalMessage) -> anyhow::Result<bool> {
        if let SignalMessage::RoomState { peers } = &msg {
            let Some(new_self) = peers.iter().find(|p| p.id == self.self_id) else {
                return Ok(true);
            };
            // Same id: just (re)join the listed peers.
            let mesh = self.mesh.as_mut().expect("mesh exists");
            let local_id = mesh.local_id().to_string();
            self.usernames = peers
                .iter()
                .filter(|p| p.id != local_id)
                .map(|p| (p.id.clone(), p.username.clone()))
                .collect();
            for peer in peers.iter().filter(|p| p.id != local_id) {
                mesh.handle_peer_joined(peer.clone()).await?;
            }
            let _ = new_self;
            return Ok(true);
        }

        let Some(mesh) = self.mesh.as_mut() else {
            return Ok(true);
        };
        match msg {
            SignalMessage::PeerJoined { peer } => {
                if peer.id == mesh.local_id() {
                    return Ok(true);
                }
                info!(peer = %peer.id, username = %peer.username, "peer joined");
                self.usernames
                    .insert(peer.id.clone(), peer.username.clone());
                if let Err(err) = mesh.handle_peer_joined(peer).await {
                    warn!(error = %err, "peer join failed");
                }
            }
            SignalMessage::PeerLeft { peer_id } => {
                info!(peer = %peer_id, "peer left");
                if let Err(err) = mesh.handle_peer_left(&peer_id).await {
                    warn!(error = %err, "peer leave failed");
                }
                self.usernames.remove(&peer_id);
                if let Some(tx) = &self.audio_control {
                    let _ = tx
                        .send(AudioControl::RemovePeer {
                            peer: peer_id.clone(),
                        })
                        .await;
                }
                self.remove_peer_tile(&peer_id);
            }
            SignalMessage::Offer { .. }
            | SignalMessage::Answer { .. }
            | SignalMessage::IceCandidate { .. }
            | SignalMessage::Renegotiate { .. } => {
                if let Err(err) = mesh.handle_signal(msg).await {
                    warn!(error = %err, "signal handling failed");
                }
            }
            SignalMessage::MediaState { peer_id, state } => {
                if peer_id != mesh.local_id() {
                    if let Err(err) = mesh
                        .handle_signal(SignalMessage::MediaState {
                            peer_id: peer_id.clone(),
                            state,
                        })
                        .await
                    {
                        warn!(error = %err, "media state handling failed");
                    }
                }
            }
            SignalMessage::ChatMessage {
                sender, content, ..
            } => {
                if sender != mesh.local_id() {
                    let username = self
                        .usernames
                        .get(&sender)
                        .cloned()
                        .unwrap_or_else(|| sender.clone());
                    self.append_chat(username, content, false);
                }
            }
            SignalMessage::RoomFull => {
                warn!("room is full");
                self.notify("Room is full".into(), "error");
                return Ok(false);
            }
            SignalMessage::Error { code, message } => {
                warn!(code = %code, "server error: {message}");
                self.notify(format!("Server error: {message}"), "error");
            }
            _ => {}
        }
        Ok(true)
    }

    /// Routes one mesh event into the UI.
    async fn handle_mesh_event(
        &mut self,
        mev: MeshEvent,
        audio_in_tx: &mpsc::Sender<(String, IncomingAudioKind, EncodedAudioFrame)>,
    ) {
        match mev {
            MeshEvent::PeerConnected { peer_id } => {
                let Some(mesh) = self.mesh.as_mut() else {
                    return;
                };
                if let Some(mut rx) = mesh.take_incoming_audio(&peer_id) {
                    info!(peer = %peer_id, "claimed incoming audio channel");
                    let tagged = audio_in_tx.clone();
                    let pid = peer_id.clone();
                    tokio::spawn(async move {
                        while let Some(frame) = rx.recv().await {
                            if tagged
                                .send((pid.clone(), IncomingAudioKind::Mic, frame))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    });
                }
                if let Some(mut rx) = mesh.take_incoming_screen_audio(&peer_id) {
                    info!(peer = %peer_id, "claimed incoming screen-audio channel");
                    let tagged = audio_in_tx.clone();
                    let pid = peer_id.clone();
                    tokio::spawn(async move {
                        while let Some(frame) = rx.recv().await {
                            if tagged
                                .send((pid.clone(), IncomingAudioKind::ScreenAudio, frame))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    });
                }
                if let Some(rx) = mesh.take_incoming_video(&peer_id) {
                    info!(peer = %peer_id, "claimed incoming video channel");
                    crate::screen_share::spawn_remote_video_forwarder(
                        rx,
                        self.window.clone(),
                        peer_id.clone(),
                    );
                }
                let username = self
                    .usernames
                    .get(&peer_id)
                    .cloned()
                    .unwrap_or_else(|| peer_id.clone());
                let initial = username
                    .chars()
                    .next()
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                self.add_peer_tile(peer_id.clone(), username, initial);
            }
            MeshEvent::PeerDisconnected { peer_id } => {
                if let Some(tx) = &self.audio_control {
                    let _ = tx
                        .send(AudioControl::RemovePeer {
                            peer: peer_id.clone(),
                        })
                        .await;
                }
                self.remove_peer_tile(&peer_id);
            }
            MeshEvent::IceStateChanged { peer_id, state } => {
                let label = ice_state_label(state);
                info!(peer = %peer_id, %label, "ice state");
                self.set_peer_tile_connection(&peer_id, label);
            }
            MeshEvent::MediaState { peer_id, state } => {
                info!(peer = %peer_id, mic = state.mic_enabled, screen = state.screen_sharing, "peer media state");
                self.set_peer_tile_muted(&peer_id, !state.mic_enabled);
                self.set_peer_tile_screen_share(&peer_id, state.screen_sharing);
            }
        }
    }

    // --- UI helpers (all mutate models on the UI thread) ---------------------

    fn add_peer_tile(&self, id: String, username: String, initial: String) {
        let window = self.window.clone();
        let _ = window.upgrade_in_event_loop(move |w| {
            if let Some(peers) = w.get_peers().as_any().downcast_ref::<VecModel<PeerData>>() {
                peers.push(PeerData {
                    id: id.into(),
                    username: username.into(),
                    initial: initial.into(),
                    frame: slint::Image::default(),
                    is_speaking: false,
                    is_muted: false,
                    has_video: false,
                    is_screen_share: false,
                    connection_state: "connecting".into(),
                });
                w.set_peer_count(peers.row_count() as i32);
            }
        });
    }

    fn remove_peer_tile(&self, id: &str) {
        let window = self.window.clone();
        let id = id.to_owned();
        let _ = window.upgrade_in_event_loop(move |w| {
            if let Some(peers) = w.get_peers().as_any().downcast_ref::<VecModel<PeerData>>() {
                for i in (0..peers.row_count()).rev() {
                    if peers.row_data(i).is_some_and(|p| p.id.as_str() == id) {
                        peers.remove(i);
                    }
                }
                w.set_peer_count(peers.row_count() as i32);
            }
        });
    }

    fn set_peer_tile_muted(&self, id: &str, muted: bool) {
        let window = self.window.clone();
        let id = id.to_owned();
        let _ = window.upgrade_in_event_loop(move |w| {
            if let Some(peers) = w.get_peers().as_any().downcast_ref::<VecModel<PeerData>>() {
                for i in 0..peers.row_count() {
                    if let Some(mut p) = peers.row_data(i) {
                        if p.id.as_str() == id {
                            p.is_muted = muted;
                            peers.set_row_data(i, p);
                        }
                    }
                }
            }
        });
    }

    fn set_peer_tile_screen_share(&self, id: &str, sharing: bool) {
        let window = self.window.clone();
        let id = id.to_owned();
        let _ = window.upgrade_in_event_loop(move |w| {
            if let Some(peers) = w.get_peers().as_any().downcast_ref::<VecModel<PeerData>>() {
                for i in 0..peers.row_count() {
                    if let Some(mut p) = peers.row_data(i) {
                        if p.id.as_str() == id {
                            p.is_screen_share = sharing;
                            peers.set_row_data(i, p);
                        }
                    }
                }
            }
        });
    }

    fn set_peer_tile_connection(&self, id: &str, state: &str) {
        let window = self.window.clone();
        let id = id.to_owned();
        let state = state.to_owned();
        let _ = window.upgrade_in_event_loop(move |w| {
            if let Some(peers) = w.get_peers().as_any().downcast_ref::<VecModel<PeerData>>() {
                for i in 0..peers.row_count() {
                    if let Some(mut p) = peers.row_data(i) {
                        if p.id.as_str() == id {
                            p.connection_state = state.clone().into();
                            peers.set_row_data(i, p);
                        }
                    }
                }
            }
        });
    }

    fn append_chat(&self, sender: String, content: String, is_self: bool) {
        let window = self.window.clone();
        let _ = window.upgrade_in_event_loop(move |w| {
            if let Some(chat) = w
                .get_chat_messages()
                .as_any()
                .downcast_ref::<VecModel<ChatMessage>>()
            {
                chat.push(ChatMessage {
                    sender: sender.into(),
                    content: content.into(),
                    timestamp: format_timestamp(now_ms()).into(),
                    is_self,
                });
            }
        });
    }

    fn notify(&self, text: String, kind: &'static str) {
        let window = self.window.clone();
        let _ = window.upgrade_in_event_loop(move |w| {
            if let Some(toasts) = w.get_toasts().as_any().downcast_ref::<VecModel<Toast>>() {
                toasts.push(Toast {
                    text: text.into(),
                    kind: kind.into(),
                });
            }
        });
        // Auto-dismiss after 4 s (Phase 4.7).
        let window = self.window.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(4)).await;
            let _ = window.upgrade_in_event_loop(|w| {
                if let Some(toasts) = w.get_toasts().as_any().downcast_ref::<VecModel<Toast>>() {
                    if toasts.row_count() > 0 {
                        toasts.remove(toasts.row_count() - 1);
                    }
                }
            });
        });
    }
}

// --- Helpers --------------------------------------------------------------

/// Waits for the first `RoomState`, which the server sends right after join.
async fn wait_for_room_state(
    events: &mut mpsc::Receiver<SignalMessage>,
    username: &str,
) -> anyhow::Result<(String, Vec<PeerInfo>)> {
    loop {
        let msg = timeout(Duration::from_secs(10), events.recv())
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn format_timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{:02}:{:02}", (secs / 60) % 60, secs % 60)
}

/// Maps an ICE transport state to the tile's connection label (drives the
/// status dot in `VideoTile`).
pub fn ice_state_label(state: IceState) -> &'static str {
    match state {
        IceState::Connected | IceState::Completed => "connected",
        IceState::Failed => "failed",
        IceState::Disconnected => "disconnected",
        _ => "connecting",
    }
}

/// Resolves the default device id (or the first device, or none).
pub fn default_device(devices: Vec<zancord_audio::devices::AudioDevice>) -> Option<String> {
    devices
        .iter()
        .find(|d| d.is_default)
        .or_else(|| devices.first())
        .map(|d| d.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ice_state_labels() {
        assert_eq!(ice_state_label(IceState::Connected), "connected");
        assert_eq!(ice_state_label(IceState::Completed), "connected");
        assert_eq!(ice_state_label(IceState::Failed), "failed");
        assert_eq!(ice_state_label(IceState::Disconnected), "disconnected");
        // Everything pre-connected maps to the amber "connecting" state.
        for state in [
            IceState::New,
            IceState::Checking,
            IceState::Disconnected,
            IceState::Closed,
        ] {
            let label = ice_state_label(state);
            assert!(
                label == "connecting" || label == "disconnected",
                "unexpected label for {state:?}: {label}"
            );
        }
    }
}
