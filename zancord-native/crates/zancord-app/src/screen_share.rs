//! Screen share pipeline (Phase 5.1): local capture → BGRA → I420 → H.264 →
//! mesh screen track (+ screen-audio track on macOS); remote mesh → H.264 →
//! I420 → RGBA → UI tile frame.
//!
//! The capture thread owns the `ScreenCapturer` (its frame/audio receivers
//! are borrowed, not owned) and does encode + pacing there; UI updates hop
//! back through `upgrade_in_event_loop`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak};
use tracing::{debug, info, warn};

use zancord_audio::codec::{OpusEncoder, FRAME_SIZE_STEREO, SCREEN_AUDIO_BITRATE};
use zancord_capture::{create_capturer, CaptureConfig};
use zancord_protocol::{EncodedVideoFrame, VideoCodec};
use zancord_transport::mesh::MeshManager;
use zancord_transport::rtcp::RtcpFeedback;
use zancord_transport::tracks::TrackKind;
use zancord_video::codec::{create_decoder, create_encoder, VideoEncoderConfig};
use zancord_video::convert::{bgra_to_i420, i420_to_rgba};

use crate::MainWindow;

/// v1 capture profile: 720p15 at ~1.2 Mbps (fast enough for software encode).
const SHARE_WIDTH: u32 = 1280;
const SHARE_HEIGHT: u32 = 720;
const SHARE_FPS: u32 = 15;
const SHARE_BITRATE: u32 = 1_200_000;
/// Force an IDR this often so late joiners / packet loss recover quickly.
const KEYFRAME_INTERVAL: Duration = Duration::from_secs(2);

/// Runs the local screen share until dropped (or `stop()` is called).
pub struct ScreenShareSession {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ScreenShareSession {
    /// Opens the platform capturer and spawns the capture/encode thread.
    pub fn start(mesh: &MeshManager, window: Weak<MainWindow>) -> Result<Self> {
        Self::start_with_channels(
            mesh.screen_tx(),
            mesh.screen_audio_tx(),
            mesh.feedback_rx(),
            window,
        )
    }

    pub fn start_with_channels(
        screen_tx: tokio::sync::mpsc::Sender<EncodedVideoFrame>,
        screen_audio_tx: tokio::sync::mpsc::Sender<zancord_protocol::EncodedAudioFrame>,
        mut feedback_rx: tokio::sync::broadcast::Receiver<RtcpFeedback>,
        window: Weak<MainWindow>,
    ) -> Result<Self> {
        let mut capturer = create_capturer()?;
        let sources = capturer.available_sources()?;
        let source = sources
            .first()
            .cloned()
            .context("no capture sources available")?;
        let config = CaptureConfig {
            width: SHARE_WIDTH,
            height: SHARE_HEIGHT,
            fps: SHARE_FPS,
            capture_audio: true,
            exclude_self_audio: true,
        };
        capturer.start_capture(&source, &config)?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);

        let handle = std::thread::Builder::new()
            .name("zancord-screen-share".to_string())
            .spawn(move || {
                let result = run_capture_loop(
                    capturer,
                    stop_flag,
                    screen_tx,
                    screen_audio_tx,
                    &mut feedback_rx,
                    window,
                );
                if let Err(err) = result {
                    warn!(error = %err, "screen share loop ended with an error");
                }
            })
            .context("failed to spawn screen share thread")?;

        info!(
            width = SHARE_WIDTH,
            height = SHARE_HEIGHT,
            fps = SHARE_FPS,
            "screen share started"
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
        info!("screen share stopped");
    }
}

impl Drop for ScreenShareSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Drains captured video + audio, encodes, and forwards to the mesh.
fn run_capture_loop(
    capturer: Box<dyn zancord_capture::ScreenCapturer>,
    stop: Arc<AtomicBool>,
    screen_tx: tokio::sync::mpsc::Sender<EncodedVideoFrame>,
    screen_audio_tx: tokio::sync::mpsc::Sender<zancord_protocol::EncodedAudioFrame>,
    feedback_rx: &mut tokio::sync::broadcast::Receiver<RtcpFeedback>,
    window: Weak<MainWindow>,
) -> Result<()> {
    let mut encoder = create_encoder(&VideoEncoderConfig {
        codec: VideoCodec::H264,
        width: SHARE_WIDTH,
        height: SHARE_HEIGHT,
        fps: SHARE_FPS,
        bitrate_bps: SHARE_BITRATE,
    })?;
    let mut audio_encoder = OpusEncoder::new_stereo(SCREEN_AUDIO_BITRATE)?;
    let mut audio_buf: Vec<i16> = Vec::with_capacity(FRAME_SIZE_STEREO * 2);
    let mut last_keyframe = Instant::now() - KEYFRAME_INTERVAL;
    let mut last_frame = Instant::now();
    let mut last_report = Instant::now();
    let (mut frames_captured, mut frames_sent, mut audio_packets_sent) = (0u64, 0u64, 0u64);
    let (mut encode_us, mut encode_count) = (0u128, 0u64);
    let (mut send_us, mut send_count) = (0u128, 0u64);

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // PLI / FIR from any peer: emit an IDR on the next frame.
        while let Ok(feedback) = feedback_rx.try_recv() {
            if let RtcpFeedback::KeyframeRequest {
                track: TrackKind::Screen,
                ..
            } = feedback
            {
                encoder.force_keyframe();
                last_keyframe = Instant::now();
            }
        }

        // Video: pace to SHARE_FPS.
        if last_frame.elapsed() >= Duration::from_millis(1000 / SHARE_FPS as u64) {
            while let Ok(frame) = capturer.video_frame_rx().try_recv() {
                frames_captured += 1;
                let (w, h) = even_dims(frame.width, frame.height);
                let yuv = match bgra_to_i420(&frame.data, w, h) {
                    Ok(yuv) => yuv,
                    Err(err) => {
                        debug!(error = %err, "skipping unencodable frame");
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
                let t1 = Instant::now();
                if screen_tx.blocking_send(encoded).is_err() {
                    bail!("screen video channel closed");
                }
                send_us += t1.elapsed().as_micros();
                send_count += 1;
                frames_sent += 1;
                // Local preview: BGRA → RGBA (byte swap).
                if let Some(preview) = bgra_to_rgba(&frame.data, w, h) {
                    post_local_preview(&window, preview, w, h);
                }
                last_frame = Instant::now();
                break; // one frame per pacing tick
            }
        }

        // System audio (macOS SCK; Linux monitor integration follows): the
        // capturer delivers interleaved f32 at 48 kHz.
        if let Some(audio_rx) = capturer.audio_sample_rx() {
            while let Ok(audio) = audio_rx.try_recv() {
                for sample in audio.pcm_data {
                    let s = (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                    audio_buf.push(s);
                    if audio_buf.len() >= FRAME_SIZE_STEREO {
                        // Chunk boundaries vary by platform (SCK / PipeWire) —
                        // send exactly one 20 ms frame and keep any overshoot
                        // for the next one instead of dropping it.
                        let frame: Vec<i16> = audio_buf.drain(..FRAME_SIZE_STEREO).collect();
                        let packet = audio_encoder.encode(&frame)?;
                        if screen_audio_tx
                            .blocking_send(zancord_protocol::EncodedAudioFrame {
                                data: packet,
                                sequence: 0,
                                timestamp_ms: 0,
                            })
                            .is_err()
                        {
                            warn!("screen audio channel closed");
                            audio_buf.clear();
                            break;
                        }
                        audio_packets_sent += 1;
                    }
                }
            }
        }

        if last_report.elapsed() >= Duration::from_secs(5) {
            let avg_ms =
                u64::try_from(encode_us / encode_count.max(1) as u128 / 1000).unwrap_or(u64::MAX);
            let send_ms =
                u64::try_from(send_us / send_count.max(1) as u128 / 1000).unwrap_or(u64::MAX);
            info!(
                frames_captured,
                frames_sent,
                audio_packets_sent,
                avg_encode_ms = avg_ms,
                avg_send_ms = send_ms,
                "screen share activity (last 5s)"
            );
            frames_captured = 0;
            frames_sent = 0;
            audio_packets_sent = 0;
            encode_us = 0;
            encode_count = 0;
            send_us = 0;
            send_count = 0;
            last_report = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// Remote video forwarder: mesh → decode → RGBA → peer tile. Spawned once per
/// connected peer (the mesh routes incoming screen video per peer).
pub fn spawn_remote_video_forwarder(
    mut rx: tokio::sync::mpsc::Receiver<EncodedVideoFrame>,
    window: Weak<MainWindow>,
    peer_id: String,
) {
    tokio::spawn(async move {
        let mut decoder = match create_decoder(VideoCodec::H264) {
            Ok(decoder) => decoder,
            Err(err) => {
                warn!(error = %err, "failed to create H.264 decoder");
                return;
            }
        };
        let mut received = 0u64;
        let mut decoded = 0u64;
        let mut reported_first = false;
        let mut last_report = Instant::now();
        while let Some(frame) = rx.recv().await {
            if last_report.elapsed() >= Duration::from_secs(5) {
                info!(
                    peer = %peer_id,
                    received,
                    decoded,
                    "video activity (last 5s): decoded=net->screen"
                );
                received = 0;
                decoded = 0;
                last_report = Instant::now();
            }
            received += 1;
            let Ok(Some(i420)) = decoder.decode(&frame.data) else {
                continue; // waiting for a keyframe / corrupt frame skipped
            };
            decoded += 1;
            if !reported_first {
                info!(peer = %peer_id, width = i420.width, height = i420.height, "first remote video frame decoded");
                reported_first = true;
            }
            let Ok(rgba) = i420_to_rgba(&i420) else {
                continue;
            };
            // `Image` is not Send in Slint, so pass raw pixels and build the
            // image on the UI thread inside the closure.
            let window = window.clone();
            let pid = peer_id.clone();
            let width = i420.width;
            let height = i420.height;
            let _ = window.upgrade_in_event_loop(move |w| {
                let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, width, height);
                let img = Image::from_rgba8(buf);
                if let Some(peers) = w
                    .get_peers()
                    .as_any()
                    .downcast_ref::<VecModel<crate::PeerData>>()
                {
                    for i in 0..peers.row_count() {
                        if let Some(mut p) = peers.row_data(i) {
                            if p.id.as_str() == pid {
                                p.frame = img;
                                p.has_video = true;
                                // `is_screen_share` is driven by MediaState
                                // (initial + live) — camera and screen share
                                // both land in this channel, so the badge must
                                // not be inferred from video arriving.
                                peers.set_row_data(i, p);
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
}

/// Crops to even dimensions (required by the I420 converters).
pub(crate) fn even_dims(width: u32, height: u32) -> (u32, u32) {
    (width & !1, height & !1)
}

/// BGRA → RGBA (just swaps R/B) for the local preview tile.
fn bgra_to_rgba(data: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let expected = width as usize * height as usize * 4;
    if data.len() != expected {
        return None;
    }
    let mut out = Vec::with_capacity(expected);
    for px in data.chunks_exact(4) {
        out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    Some(out)
}

/// Pushes a local preview frame into the self-view tile.
pub(crate) fn post_local_preview(
    window: &Weak<MainWindow>,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
) {
    // `Image` is not Send in Slint — build it on the UI thread.
    let window = window.clone();
    let _ = window.upgrade_in_event_loop(move |w| {
        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, width, height);
        w.set_local_video_frame(Image::from_rgba8(buf));
        w.set_local_has_video(true);
    });
}
