//! macOS ScreenCaptureKit implementation (Phase 3A.2).
//!
//! Requirements:
//! - Screen-recording TCC permission (System Settings → Privacy & Security →
//!   Screen Recording) for the running binary.
//! - `NSScreenCaptureUsageDescription` in the app's Info.plist. For dev
//!   binaries, ad-hoc signing: `codesign --force --sign - <binary>`.
//!
//! Frames are delivered as packed BGRA (row padding stripped). System audio
//! (Phase 3B) arrives as Float32 PCM via the same stream when
//! `CaptureConfig::capture_audio` is set.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use screencapturekit::cm::{AudioBufferList, CMSampleBuffer, CMTime};
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;
use screencapturekit::stream::configuration::PixelFormat as ScPixelFormat;
use tracing::{debug, error, info};

use crate::traits::{
    CaptureConfig, CaptureSource, CaptureSourceType, CapturedAudioFrame, CapturedVideoFrame,
    PixelFormat, ScreenCapturer,
};

/// System audio is requested at this rate; ScreenCaptureKit delivers Float32.
const AUDIO_SAMPLE_RATE: u32 = 48_000;
/// Stereo, matching the app's audio pipeline.
const AUDIO_CHANNELS: u16 = 2;

// --- TCC permission (CoreGraphics FFI) -------------------------------------
// Deprecated since macOS 15 (ScreenCaptureKit has no request API of its own)
// but still functional on macOS 26.
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// True when the current process already has screen-recording permission.
pub fn has_screen_capture_permission() -> bool {
    // SAFETY: plain framework calls with no arguments and no pointers.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Requests screen-recording permission, returning true when granted. Shows
/// the system TCC prompt the first time (requires `NSScreenCaptureUsageDescription`
/// in the app's Info.plist for bundled apps).
pub fn request_screen_capture_permission() -> bool {
    // SAFETY: plain framework call with no arguments.
    unsafe { CGRequestScreenCaptureAccess() }
}

/// Ensures screen-recording permission, prompting the user when needed.
pub fn ensure_screen_capture_permission() -> Result<()> {
    if has_screen_capture_permission() {
        return Ok(());
    }
    info!("requesting screen recording permission (TCC prompt)");
    if request_screen_capture_permission() {
        Ok(())
    } else {
        bail!(
            "screen recording permission denied — enable it in System Settings → \
             Privacy & Security → Screen Recording"
        )
    }
}

/// ScreenCaptureKit-backed capturer. The output handler runs on ScreenCaptureKit's
/// dispatch queue, so all frame/audio work happens off the caller's thread.
pub struct MacScreenCapturer {
    video_tx: Sender<CapturedVideoFrame>,
    video_rx: Receiver<CapturedVideoFrame>,
    audio_tx: Option<Sender<CapturedAudioFrame>>,
    audio_rx: Option<Receiver<CapturedAudioFrame>>,
    stream: Option<SCStream>,
    /// Delegate-reported stop error (permission revoked, source gone, …).
    stream_error: Arc<Mutex<Option<String>>>,
    config: CaptureConfig,
}

impl MacScreenCapturer {
    pub fn new() -> Self {
        let (video_tx, video_rx) = mpsc::channel();
        Self {
            video_tx,
            video_rx,
            audio_tx: None,
            audio_rx: None,
            stream: None,
            stream_error: Arc::new(Mutex::new(None)),
            config: CaptureConfig::default(),
        }
    }

    /// Adjusts resolution/fps while the stream is running (SCK re-configures
    /// in place; the audio parts of `config` are ignored here).
    pub fn update_config(&mut self, config: &CaptureConfig) -> Result<()> {
        let Some(stream) = &self.stream else {
            bail!("capture is not running");
        };
        let stream_config = video_config(config);
        stream
            .update_configuration(&stream_config)
            .context("failed to update capture configuration")?;
        self.config = config.clone();
        debug!(
            width = config.width,
            height = config.height,
            fps = config.fps,
            "capture config updated"
        );
        Ok(())
    }

    /// The current capture configuration (updated by `update_config`).
    pub fn config(&self) -> &CaptureConfig {
        &self.config
    }
}

impl Default for MacScreenCapturer {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenCapturer for MacScreenCapturer {
    fn available_sources(&self) -> Result<Vec<CaptureSource>> {
        let content = SCShareableContent::get()
            .context("SCShareableContent::get failed (screen recording permission missing?)")?;
        let mut sources = Vec::new();
        for display in content.displays() {
            sources.push(CaptureSource {
                id: format!("display:{}", display.display_id()),
                name: format!("Display {}", display.display_id()),
                source_type: CaptureSourceType::Display,
                thumbnail: None,
            });
        }
        for window in content.windows().iter().filter(|w| w.is_on_screen()) {
            let app = window
                .owning_application()
                .map(|a| a.application_name())
                .unwrap_or_default();
            let title = window.title().unwrap_or_else(|| "Untitled".to_owned());
            sources.push(CaptureSource {
                id: format!("window:{}", window.window_id()),
                name: if app.is_empty() {
                    title
                } else {
                    format!("{app} — {title}")
                },
                source_type: CaptureSourceType::Window,
                thumbnail: None,
            });
        }
        Ok(sources)
    }

    fn start_capture(&mut self, source: &CaptureSource, config: &CaptureConfig) -> Result<()> {
        ensure_screen_capture_permission()?;
        if self.stream.is_some() {
            self.stop_capture()?;
        }

        let content = SCShareableContent::get()
            .context("SCShareableContent::get failed (screen recording permission missing?)")?;
        let filter = match source.source_type {
            CaptureSourceType::Display => {
                let id = source
                    .id
                    .strip_prefix("display:")
                    .context("malformed display source id")?
                    .parse::<u32>()
                    .context("malformed display id")?;
                let display = content
                    .displays()
                    .into_iter()
                    .find(|d| d.display_id() == id)
                    .context("display not found (disconnected?)")?;
                SCContentFilter::create()
                    .with_display(&display)
                    .with_excluding_windows(&[])
                    .build()
            }
            CaptureSourceType::Window => {
                let id = source
                    .id
                    .strip_prefix("window:")
                    .context("malformed window source id")?
                    .parse::<u32>()
                    .context("malformed window id")?;
                let window = content
                    .windows()
                    .into_iter()
                    .find(|w| w.window_id() == id)
                    .context("window not found (closed?)")?;
                SCContentFilter::create().with_window(&window).build()
            }
            CaptureSourceType::Application => {
                bail!("application capture is not supported yet")
            }
        };

        let stream_config = video_config(config);
        let stream_config = if config.capture_audio {
            stream_config
                .with_captures_audio(true)
                .with_sample_rate(AUDIO_SAMPLE_RATE as i32)
                .with_channel_count(AUDIO_CHANNELS as i32)
                .with_excludes_current_process_audio(config.exclude_self_audio)
        } else {
            stream_config
        };

        // The audio channel exists only when the current config captures audio.
        if config.capture_audio && self.audio_rx.is_none() {
            let (audio_tx, audio_rx) = mpsc::channel();
            self.audio_tx = Some(audio_tx);
            self.audio_rx = Some(audio_rx);
        } else if !config.capture_audio {
            self.audio_tx = None;
            self.audio_rx = None;
        }

        let video_tx = self.video_tx.clone();
        let audio_tx = self.audio_tx.clone();
        let handler = move |sample: CMSampleBuffer, of_type: SCStreamOutputType| match of_type {
            SCStreamOutputType::Screen => push_video_frame(&video_tx, &sample),
            SCStreamOutputType::Audio => {
                if let Some(tx) = &audio_tx {
                    push_audio_frame(tx, &sample);
                }
            }
            _ => {}
        };

        let stream_error = Arc::clone(&self.stream_error);
        let delegate = ErrorHandler::new(move |err: SCError| {
            error!("screen capture stream stopped: {err}");
            *stream_error.lock().expect("stream_error lock") = Some(err.to_string());
        });

        let mut stream = SCStream::new_with_delegate(&filter, &stream_config, delegate);
        stream.add_output_handler(handler.clone(), SCStreamOutputType::Screen);
        if config.capture_audio {
            stream.add_output_handler(handler, SCStreamOutputType::Audio);
        }
        stream
            .start_capture()
            .context("failed to start screen capture")?;

        self.stream = Some(stream);
        self.config = config.clone();
        info!(
            source = %source.name,
            width = config.width,
            height = config.height,
            fps = config.fps,
            audio = config.capture_audio,
            "screen capture started"
        );
        Ok(())
    }

    fn stop_capture(&mut self) -> Result<()> {
        let reported = self.stream_error.lock().expect("stream_error lock").take();
        if let Some(stream) = self.stream.take() {
            if let Err(err) = stream.stop_capture() {
                if reported.is_none() {
                    return Err(err).context("failed to stop capture");
                }
                // The stream already died on its own (delegate reported it);
                // failing to stop a dead stream is expected.
                debug!("stop_capture on dead stream: {err}");
            }
        }
        if let Some(err) = reported {
            bail!("capture stream ended with an error: {err}");
        }
        info!("screen capture stopped");
        Ok(())
    }

    fn video_frame_rx(&self) -> &Receiver<CapturedVideoFrame> {
        &self.video_rx
    }

    fn audio_sample_rx(&self) -> Option<&Receiver<CapturedAudioFrame>> {
        self.audio_rx.as_ref()
    }
}

/// Builds the video part of the stream configuration (BGRA, capped fps).
fn video_config(config: &CaptureConfig) -> SCStreamConfiguration {
    let frame_interval = CMTime::new(1, config.fps.clamp(1, 120) as i32);
    SCStreamConfiguration::new()
        .with_width(config.width.max(1))
        .with_height(config.height.max(1))
        .with_pixel_format(ScPixelFormat::BGRA)
        .with_minimum_frame_interval(&frame_interval)
        .with_shows_cursor(true)
}

/// Converts a captured pixel buffer to a packed BGRA frame (row padding
/// stripped) and sends it on the channel.
fn push_video_frame(tx: &Sender<CapturedVideoFrame>, sample: &CMSampleBuffer) {
    // SCK delivery telemetry: how many frames the OS actually hands us.
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    static DELIVERED: AtomicU64 = AtomicU64::new(0);
    static LAST_LOG: Mutex<Option<std::time::Instant>> = Mutex::new(None);

    let Some(pb) = sample.image_buffer() else {
        return;
    };
    let Ok(guard) = pb.lock(CVPixelBufferLockFlags::READ_ONLY) else {
        debug!("could not lock pixel buffer");
        return;
    };
    let delivered = DELIVERED.fetch_add(1, Ordering::Relaxed) + 1;
    let now = std::time::Instant::now();
    if let Ok(mut last) = LAST_LOG.lock() {
        if last.map_or(true, |t| {
            now.duration_since(t) >= std::time::Duration::from_secs(5)
        }) {
            info!(delivered, "screen capture frames delivered by SCK");
            *last = Some(now);
        }
    }
    let width = guard.width() as u32;
    let height = guard.height() as u32;
    let bpr = guard.bytes_per_row();
    let row_bytes = width as usize * 4;
    if width == 0 || height == 0 || bpr < row_bytes {
        return;
    }
    let slice = guard.as_slice();
    let mut data = Vec::with_capacity(height as usize * row_bytes);
    for row in 0..height as usize {
        let start = row * bpr;
        data.extend_from_slice(&slice[start..start + row_bytes]);
    }
    let _ = tx.send(CapturedVideoFrame {
        data,
        width,
        height,
        pixel_format: PixelFormat::Bgra,
        timestamp_us: cmtime_to_us(sample.presentation_timestamp()),
    });
}

/// Extracts Float32 PCM from an `AudioBufferList` and sends it on the channel.
/// Handles both non-interleaved (one buffer per channel, as SCK delivers) and
/// interleaved single-buffer layouts.
fn push_audio_frame(tx: &Sender<CapturedAudioFrame>, sample: &CMSampleBuffer) {
    let Some(list) = sample.audio_buffer_list() else {
        return;
    };
    let Some(frame) = audio_list_to_frame(&list) else {
        return;
    };
    let _ = tx.send(frame);
}

fn audio_list_to_frame(list: &AudioBufferList) -> Option<CapturedAudioFrame> {
    let buffers: Vec<&[u8]> = list.iter().map(|b| b.data()).collect();
    let (pcm_data, channels) = interleave_audio(&buffers)?;
    Some(CapturedAudioFrame {
        pcm_data,
        sample_rate: AUDIO_SAMPLE_RATE,
        channels,
        timestamp_us: 0,
    })
}

/// Converts raw f32 PCM byte buffers to interleaved samples. Handles both
/// non-interleaved layouts (one buffer per channel, as SCK delivers) and a
/// single interleaved buffer. Returns `None` for empty input.
fn interleave_audio(buffers: &[&[u8]]) -> Option<(Vec<f32>, u16)> {
    if buffers.is_empty() {
        return None;
    }
    let channels = if buffers.len() == 1 {
        AUDIO_CHANNELS
    } else {
        buffers.len().min(u16::MAX as usize) as u16
    };
    let mut pcm = Vec::new();
    if buffers.len() > 1 {
        // Non-interleaved: one buffer per channel → interleave.
        let frames = buffers.iter().map(|b| b.len() / 4).min().unwrap_or(0);
        for i in 0..frames {
            for b in buffers {
                pcm.push(f32::from_le_bytes([
                    b[i * 4],
                    b[i * 4 + 1],
                    b[i * 4 + 2],
                    b[i * 4 + 3],
                ]));
            }
        }
    } else {
        // Interleaved single buffer.
        for chunk in buffers[0].chunks_exact(4) {
            pcm.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    }
    Some((pcm, channels))
}

/// CMTime → microseconds (invalid/negative times become 0).
fn cmtime_to_us(t: CMTime) -> u64 {
    if t.timescale <= 0 {
        return 0;
    }
    let us = (t.value as i128 * 1_000_000) / t.timescale as i128;
    us.max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Live tests need the terminal/IDE running the tests to hold screen
    /// recording permission; gate them behind an explicit env var so `cargo
    /// test` stays non-interactive.
    fn live_tests_enabled() -> bool {
        std::env::var("ZANCORD_CAPTURE_TEST").as_deref() == Ok("1")
    }

    #[test]
    fn cmtime_to_microseconds() {
        assert_eq!(cmtime_to_us(CMTime::new(1_000_000, 1_000_000)), 1_000_000);
        assert_eq!(cmtime_to_us(CMTime::new(48_000, 48_000)), 1_000_000);
        assert_eq!(cmtime_to_us(CMTime::new(45_000, 90_000)), 500_000);
        assert_eq!(cmtime_to_us(CMTime::new(1_000, 90_000)), 11_111);
        assert_eq!(cmtime_to_us(CMTime::new(0, 0)), 0); // invalid
        assert_eq!(cmtime_to_us(CMTime::new(-5, 10)), 0); // negative
    }

    #[test]
    fn interleaves_non_interleaved_audio() {
        // Two channels, 2 frames each, non-interleaved (one buffer per channel).
        let ch0: Vec<u8> = vec![1.0f32, 2.0]
            .into_iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let ch1: Vec<u8> = vec![3.0f32, 4.0]
            .into_iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let (pcm, channels) = interleave_audio(&[&ch0, &ch1]).expect("frame");
        assert_eq!(channels, 2);
        assert_eq!(pcm, vec![1.0, 3.0, 2.0, 4.0]);
    }

    #[test]
    fn passes_through_interleaved_audio() {
        // Single buffer already carrying stereo interleaved samples.
        let stereo: Vec<u8> = vec![1.0f32, 2.0, 3.0, 4.0]
            .into_iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let (pcm, channels) = interleave_audio(&[&stereo]).expect("frame");
        assert_eq!(channels, 2);
        assert_eq!(pcm, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn empty_audio_is_none() {
        assert!(interleave_audio(&[]).is_none());
    }

    #[test]
    fn enumerates_at_least_one_display() {
        if !live_tests_enabled() {
            eprintln!(
                "skipping live capture test — set ZANCORD_CAPTURE_TEST=1 and grant \
                 screen recording permission to the terminal"
            );
            return;
        }
        let capturer = MacScreenCapturer::new();
        let sources = capturer.available_sources().expect("sources");
        assert!(
            sources
                .iter()
                .any(|s| s.source_type == CaptureSourceType::Display),
            "at least one display must be enumerable, got {sources:?}"
        );
    }

    #[test]
    fn captures_frames_and_stops_cleanly() {
        if !live_tests_enabled() {
            eprintln!("skipping live capture test — set ZANCORD_CAPTURE_TEST=1");
            return;
        }
        let mut capturer = MacScreenCapturer::new();
        let sources = capturer.available_sources().expect("sources");
        let display = sources
            .iter()
            .find(|s| s.source_type == CaptureSourceType::Display)
            .expect("a display source");
        let config = CaptureConfig {
            width: 1280,
            height: 720,
            fps: 15,
            capture_audio: false,
            exclude_self_audio: true,
        };
        capturer
            .start_capture(display, &config)
            .expect("capture starts");

        let mut frames = 0u32;
        let deadline = Instant::now() + Duration::from_secs(10);
        while frames < 10 && Instant::now() < deadline {
            while let Ok(frame) = capturer.video_frame_rx().try_recv() {
                assert_eq!(frame.pixel_format, PixelFormat::Bgra);
                assert_eq!(
                    frame.data.len(),
                    frame.width as usize * frame.height as usize * 4,
                    "packed BGRA"
                );
                frames += 1;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        capturer.stop_capture().expect("capture stops cleanly");
        assert!(frames >= 10, "expected >= 10 frames, got {frames}");
    }

    #[test]
    fn config_change_while_capturing() {
        if !live_tests_enabled() {
            eprintln!("skipping live capture test — set ZANCORD_CAPTURE_TEST=1");
            return;
        }
        let mut capturer = MacScreenCapturer::new();
        let sources = capturer.available_sources().expect("sources");
        let display = sources
            .iter()
            .find(|s| s.source_type == CaptureSourceType::Display)
            .expect("a display source");
        capturer
            .start_capture(
                display,
                &CaptureConfig {
                    width: 1280,
                    height: 720,
                    fps: 15,
                    capture_audio: false,
                    exclude_self_audio: true,
                },
            )
            .expect("capture starts");

        // Wait for at least one frame at the initial resolution, then switch
        // to a lower resolution while the stream is running.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && capturer.video_frame_rx().try_recv().is_err() {
            std::thread::sleep(Duration::from_millis(50));
        }
        capturer
            .update_config(&CaptureConfig {
                width: 640,
                height: 360,
                fps: 10,
                capture_audio: false,
                exclude_self_audio: true,
            })
            .expect("config updates while running");

        let mut saw_new_size = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !saw_new_size {
            while let Ok(frame) = capturer.video_frame_rx().try_recv() {
                if frame.width == 640 && frame.height == 360 {
                    saw_new_size = true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        capturer.stop_capture().expect("capture stops cleanly");
        assert!(saw_new_size, "frames at the new resolution must arrive");
    }
}
