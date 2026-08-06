//! Camera pipeline (Phase 5.2): local webcam → RGB24 → I420 → H.264 → mesh
//! camera track; local preview (mirrored, the self-view convention) into the
//! self-view tile.
//!
//! The capture thread owns the `CameraCapturer` (nokhwa runs the device on its
//! own thread and pushes decoded RGB frames into a channel); encode + pacing
//! happen on the capture thread, and UI updates hop back through
//! `upgrade_in_event_loop`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use slint::Weak;
use tracing::{debug, info, warn};

use zancord_capture::{create_camera, CameraCapturer, CameraConfig};
use zancord_protocol::{EncodedVideoFrame, VideoCodec};
use zancord_transport::mesh::MeshManager;
use zancord_transport::rtcp::RtcpFeedback;
use zancord_transport::tracks::TrackKind;
use zancord_video::codec::{create_encoder, VideoEncoderConfig};
use zancord_video::convert::rgb_to_i420;

use crate::bitrate::CongestionState;
use crate::screen_share::{even_dims, post_local_preview};
use crate::MainWindow;

/// v1 camera profile: 720p30 at ~2 Mbps (software H.264 encode budget).
const CAM_WIDTH: u32 = 1280;
const CAM_HEIGHT: u32 = 720;
const CAM_FPS: u32 = 30;
const CAM_BITRATE: u32 = 2_000_000;
/// Force an IDR this often so late joiners / packet loss recover quickly.
const KEYFRAME_INTERVAL: Duration = Duration::from_secs(2);

/// Runs the local camera until dropped (or `stop()` is called).
pub struct CameraSession {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl CameraSession {
    /// Opens the webcam (`camera_index`, see `available_cameras`) and spawns
    /// the capture/encode thread.
    pub fn start(mesh: &MeshManager, window: Weak<MainWindow>, camera_index: u32) -> Result<Self> {
        Self::start_with_channels(mesh.camera_tx(), mesh.feedback_rx(), window, camera_index)
    }

    pub fn start_with_channels(
        camera_tx: tokio::sync::mpsc::Sender<EncodedVideoFrame>,
        mut feedback_rx: tokio::sync::broadcast::Receiver<RtcpFeedback>,
        window: Weak<MainWindow>,
        camera_index: u32,
    ) -> Result<Self> {
        let config = CameraConfig {
            width: CAM_WIDTH,
            height: CAM_HEIGHT,
            fps: CAM_FPS,
            index: camera_index,
        };
        // Opening the device can block (AVFoundation handshake) — the caller
        // already runs this on a blocking task; the loop thread then owns the
        // capturer for the session's lifetime.
        let mut capturer = create_camera(&config)?;
        let camera_name = capturer.name();

        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);

        let handle = std::thread::Builder::new()
            .name("zancord-camera".to_string())
            .spawn(move || {
                if let Err(err) = run_capture_loop(
                    &mut *capturer,
                    stop_flag,
                    camera_tx,
                    &mut feedback_rx,
                    window,
                ) {
                    warn!(error = %err, "camera loop ended with an error");
                }
            })
            .context("failed to spawn camera thread")?;

        info!(
            camera = %camera_name,
            width = CAM_WIDTH,
            height = CAM_HEIGHT,
            fps = CAM_FPS,
            "camera started"
        );
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }

    /// Stops capture and joins the thread.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        info!("camera stopped");
    }
}

impl Drop for CameraSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Drains captured frames, encodes, and forwards to the mesh. The camera
/// delivers frames on its own thread, so `next_frame` blocks at the device
/// rate; frames the encoder can't keep up with are skipped by the pacing gate.
fn run_capture_loop(
    capturer: &mut dyn CameraCapturer,
    stop: Arc<AtomicBool>,
    camera_tx: tokio::sync::mpsc::Sender<EncodedVideoFrame>,
    feedback_rx: &mut tokio::sync::broadcast::Receiver<RtcpFeedback>,
    window: Weak<MainWindow>,
) -> Result<()> {
    let mut encoder = create_encoder(&VideoEncoderConfig {
        codec: VideoCodec::H264,
        width: CAM_WIDTH,
        height: CAM_HEIGHT,
        fps: CAM_FPS,
        bitrate_bps: CAM_BITRATE,
    })?;
    let mut congestion = CongestionState::new(CAM_BITRATE);
    let mut last_keyframe = Instant::now() - KEYFRAME_INTERVAL;
    let mut last_frame = Instant::now();
    let mut last_report = Instant::now();
    let (mut frames_captured, mut frames_encoded, mut frames_sent) = (0u64, 0u64, 0u64);
    let (mut encode_us, mut encode_count) = (0u128, 0u64);
    let mut frames_since_encoded = 0u32;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // PLI / FIR from any peer: emit an IDR on the next frame. REMB from
        // the slowest receiver drives the congestion policy (frame-skip below).
        while let Ok(feedback) = feedback_rx.try_recv() {
            match feedback {
                RtcpFeedback::KeyframeRequest {
                    track: TrackKind::Camera,
                    ..
                } => {
                    encoder.force_keyframe();
                    last_keyframe = Instant::now();
                }
                RtcpFeedback::BitrateHint {
                    track: TrackKind::Camera,
                    peer_id,
                    bitrate_bps,
                } => {
                    let policy = congestion.update(&peer_id, bitrate_bps, Instant::now());
                    encoder.set_bitrate(policy.encoder_bps);
                }
                _ => {}
            }
        }

        let Some(frame) = capturer.next_frame()? else {
            break; // camera closed (stop or device lost)
        };
        frames_captured += 1;

        // Pace to CAM_FPS: skip frames while the encoder is behind.
        if last_frame.elapsed() < Duration::from_millis(1000 / CAM_FPS as u64) {
            continue;
        }
        last_frame = Instant::now();
        // Congestion control: send 1 of every N frames while the slowest
        // receiver's REMB is below our target.
        frames_since_encoded += 1;
        if frames_since_encoded % congestion.policy(Instant::now()).frame_skip != 0 {
            continue;
        }
        frames_since_encoded = 0;

        let (w, h) = even_dims(frame.width, frame.height);
        let yuv = match rgb_to_i420(&frame.data, w, h) {
            Ok(yuv) => yuv,
            Err(err) => {
                debug!(error = %err, "skipping unencodable camera frame");
                continue;
            }
        };
        if last_keyframe.elapsed() >= KEYFRAME_INTERVAL {
            encoder.force_keyframe();
            last_keyframe = Instant::now();
        }
        let t0 = Instant::now();
        let encoded = encoder.encode(&yuv)?;
        encode_us += t0.elapsed().as_micros();
        encode_count += 1;
        if camera_tx.blocking_send(encoded).is_err() {
            bail!("camera video channel closed");
        }
        frames_encoded += 1;
        frames_sent += 1;

        // Local preview: RGB → RGBA, mirrored (self-view convention). The mesh
        // gets the unmirrored frame.
        if let Some(preview) = rgb_to_rgba_mirrored(&frame.data, w, h) {
            post_local_preview(&window, preview, w, h);
        }

        if last_report.elapsed() >= Duration::from_secs(5) {
            let avg_ms =
                u64::try_from(encode_us / encode_count.max(1) as u128 / 1000).unwrap_or(u64::MAX);
            info!(
                frames_captured,
                frames_encoded,
                frames_sent,
                avg_encode_ms = avg_ms,
                "camera activity (last 5s)"
            );
            frames_captured = 0;
            frames_encoded = 0;
            frames_sent = 0;
            encode_us = 0;
            encode_count = 0;
            last_report = Instant::now();
        }
    }
    Ok(())
}

/// RGB24 → RGBA with a horizontal flip (local preview only).
fn rgb_to_rgba_mirrored(data: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let expected = width as usize * height as usize * 3;
    if data.len() != expected || width == 0 {
        return None;
    }
    let w = width as usize;
    let h = height as usize;
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        for col in 0..w {
            let src = (row * w + (w - 1 - col)) * 3;
            out.extend_from_slice(&[data[src + 2], data[src + 1], data[src], 255]);
        }
    }
    Some(out)
}
