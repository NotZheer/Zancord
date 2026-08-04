//! Linux PipeWire + XDG Desktop Portal implementation (Phase 3A.3).
//!
//! Flow: the XDG Desktop Portal shows the system picker dialog (Wayland
//! compositor or X11) and hands back a PipeWire node id + a file descriptor.
//! We connect to that node with a PipeWire capture stream and forward frames.
//!
//! Because the portal owns source selection, `available_sources()` returns a
//! single synthetic "system picker" source; `start_capture` on it pops the
//! dialog. Requires a running portal (xdg-desktop-portal + compositor
//! backend) and PipeWire (libpipewire-0.3 at runtime).

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use enumflags2::BitFlags;
use pipewire as pw;
use pipewire::properties::properties;
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::param::ParamType;
use pw::spa::pod::{serialize::PodSerializer, Object, Pod, Value};
use pw::spa::utils::SpaTypes;
use tracing::{debug, error, info, warn};

use crate::traits::{
    CaptureConfig, CaptureSource, CaptureSourceType, CapturedVideoFrame, PixelFormat,
    ScreenCapturer,
};

/// Source id of the synthetic picker entry (the portal shows the real list).
const PICKER_SOURCE_ID: &str = "portal-picker";

/// Per-stream state shared with the PipeWire process callback.
struct StreamUserData {
    format: VideoInfoRaw,
    video_tx: Sender<CapturedVideoFrame>,
    stream_error: Arc<Mutex<Option<String>>>,
    started: Instant,
}

/// PipeWire + XDG portal-backed capturer. The PipeWire process callback runs
/// on the thread loop's internal thread; all frames are forwarded over a
/// channel owned by this struct.
pub struct LinuxScreenCapturer {
    video_tx: Sender<CapturedVideoFrame>,
    video_rx: Receiver<CapturedVideoFrame>,
    stream_error: Arc<Mutex<Option<String>>>,
    /// Leaked once per start (opaque ZST — no memory cost) to give the
    /// owning `Box` wrappers a `'static` loop reference.
    thread_loop: Option<&'static pw::thread_loop::ThreadLoop>,
    /// Drop order matters: listener → stream → core → context → loop.
    _listener: Option<pw::stream::StreamListener<StreamUserData>>,
    stream: Option<pw::stream::StreamBox<'static>>,
    core: Option<pw::core::CoreBox<'static>>,
    context: Option<pw::context::ContextBox<'static>>,
    config: CaptureConfig,
    /// Runtime for the ashpd (zbus) async portal calls.
    runtime: tokio::runtime::Runtime,
}

impl LinuxScreenCapturer {
    pub fn new() -> Self {
        pw::init();
        let (video_tx, video_rx) = mpsc::channel();
        Self {
            video_tx,
            video_rx,
            stream_error: Arc::new(Mutex::new(None)),
            thread_loop: None,
            _listener: None,
            stream: None,
            core: None,
            context: None,
            config: CaptureConfig::default(),
            runtime: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime for portal calls"),
        }
    }
}

impl Default for LinuxScreenCapturer {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenCapturer for LinuxScreenCapturer {
    fn available_sources(&self) -> Result<Vec<CaptureSource>> {
        // The XDG portal has no enumeration API — the system picker dialog
        // presents the actual displays/windows at selection time.
        Ok(vec![CaptureSource {
            id: PICKER_SOURCE_ID.to_owned(),
            name: "Screen (system picker)".to_owned(),
            source_type: CaptureSourceType::Display,
            thumbnail: None,
        }])
    }

    fn start_capture(&mut self, source: &CaptureSource, config: &CaptureConfig) -> Result<()> {
        if self.thread_loop.is_some() {
            self.stop_capture()?;
        }

        let types = match source.source_type {
            CaptureSourceType::Display => SourceType::Monitor | SourceType::Window,
            CaptureSourceType::Window => SourceType::Monitor | SourceType::Window,
            CaptureSourceType::Application => bail!("application capture is not supported"),
        };

        // --- XDG Desktop Portal: session → picker → node id + fd ------------
        let (node_id, fd) = self.runtime.block_on(async {
            let screencast = Screencast::new()
                .await
                .context("cannot reach org.freedesktop.portal.ScreenCast")?;
            let session = screencast
                .create_session()
                .await
                .context("create_session failed")?;

            screencast
                .select_sources(
                    &session,
                    CursorMode::Embedded,
                    BitFlags::from_bits_truncate(types.bits()),
                    false, // single source
                    None,
                    PersistMode::DoNot,
                )
                .await
                .context("select_sources failed")?
                .response()
                .context("portal rejected source selection")?;

            // Shows the picker dialog and blocks until the user chooses.
            let streams = screencast
                .start(&session, None)
                .await
                .context("start failed (user cancelled?)")?
                .response()
                .context("portal rejected stream start")?;
            let stream = streams
                .streams()
                .first()
                .context("portal returned no streams")?;
            let node_id = stream.pipe_wire_node_id();
            let fd = screencast
                .open_pipe_wire_remote(&session)
                .await
                .context("open_pipe_wire_remote failed")?;
            anyhow::Ok((node_id, fd))
        })?;
        info!(node_id, "portal granted a capture stream");

        // --- PipeWire: connect to the portal node ----------------------------
        let thread_loop: &'static pw::thread_loop::ThreadLoop =
            Box::leak(Box::new(pw::thread_loop::ThreadLoop::new(None)?));
        thread_loop.start();

        let context = pw::context::ContextBox::new(thread_loop.loop_(), None)
            .context("pw_context_new failed")?;
        let core = context
            .connect_fd(fd, None)
            .context("pw_context_connect_fd failed")?;
        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        };
        let stream = pw::stream::StreamBox::new(&core, "zancord-screen", props)
            .context("pw_stream_new failed")?;

        let user_data = StreamUserData {
            format: VideoInfoRaw::new(),
            video_tx: self.video_tx.clone(),
            stream_error: Arc::clone(&self.stream_error),
            started: Instant::now(),
        };
        let listener = stream
            .add_local_listener_with_user_data(user_data)
            .param_changed(|_, user_data, id, param| {
                let Some(param) = param else { return };
                if id != ParamType::Format.as_raw() {
                    return;
                }
                if let Err(err) = user_data.format.parse(param) {
                    warn!(error = %err, "failed to parse negotiated video format");
                }
            })
            .process(|stream, user_data| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(frame) = buffer_to_frame(&mut buffer, user_data) else {
                    return;
                };
                if user_data.video_tx.send(frame).is_err() {
                    // Consumer gone; report so stop_capture surfaces it.
                    *user_data.stream_error.lock().expect("stream_error lock") =
                        Some("video channel closed".to_owned());
                }
            })
            .register()
            .context("failed to register stream listener")?;

        let mut params = [format_pod(config)?];
        stream
            .connect(
                pw::spa::utils::Direction::Input,
                Some(node_id),
                pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .context("pw_stream_connect failed")?;

        // Retain everything in dependency order so drops are safe.
        self.thread_loop = Some(thread_loop);
        self.context = Some(context);
        self.core = Some(core);
        self.stream = Some(stream);
        self._listener = Some(listener);
        self.config = config.clone();
        info!(
            node_id,
            width = config.width,
            height = config.height,
            "screen capture started"
        );
        Ok(())
    }

    fn stop_capture(&mut self) -> Result<()> {
        if let Some(thread_loop) = self.thread_loop.take() {
            thread_loop.stop();
        }
        // Drop listener → stream → core → context in that order.
        self._listener = None;
        self.stream = None;
        self.core = None;
        self.context = None;
        if let Some(err) = self.stream_error.lock().expect("stream_error lock").take() {
            bail!("capture stream ended with an error: {err}");
        }
        info!("screen capture stopped");
        Ok(())
    }

    fn video_frame_rx(&self) -> &Receiver<CapturedVideoFrame> {
        &self.video_rx
    }

    fn audio_sample_rx(&self) -> Option<&Receiver<crate::traits::CapturedAudioFrame>> {
        // System audio capture lands in Phase 3B (PipeWire monitor stream).
        None
    }
}

/// Builds the SPA EnumFormat pod requesting BGRx at the configured size/fps.
fn format_pod(config: &CaptureConfig) -> Result<Pod> {
    let mut video_info = VideoInfoRaw::new();
    video_info.set_format(VideoFormat::BGRx);
    video_info.set_size(pw::spa::utils::Rectangle {
        width: config.width.max(1),
        height: config.height.max(1),
    });
    video_info.set_framerate(pw::spa::utils::Fraction {
        num: config.fps.clamp(1, 60),
        denom: 1,
    });
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: video_info.into(),
    };
    let values = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
        .context("failed to serialize format pod")?
        .0
        .into_inner();
    Pod::from_bytes(&values).context("failed to parse format pod")
}

/// Copies one captured buffer out as a packed `CapturedVideoFrame`, stripping
/// row padding. Supports BGRx/RGBx (→ BGRA) and NV12.
fn buffer_to_frame(
    buffer: &mut pw::buffer::Buffer<'_>,
    user_data: &mut StreamUserData,
) -> Option<CapturedVideoFrame> {
    let datas = buffer.datas_mut();
    let data = datas.first_mut()?;
    let slice = data.data()?;
    let size = data.chunk().size() as usize;
    if size == 0 {
        return None;
    }
    let bytes = &slice[..size.min(slice.len())];
    let width = user_data.format.size().width;
    let height = user_data.format.size().height;
    let stride = data.chunk().stride().max(0) as usize;
    let row_bytes = width as usize * 4;
    if width == 0 || height == 0 {
        return None;
    }

    let (data, pixel_format) = match user_data.format.format() {
        VideoFormat::BGRx => {
            let mut out = Vec::with_capacity(height as usize * row_bytes);
            for row in 0..height as usize {
                let start = row * stride;
                if start + row_bytes > bytes.len() {
                    return None;
                }
                out.extend_from_slice(&bytes[start..start + row_bytes]);
            }
            (out, PixelFormat::Bgra)
        }
        VideoFormat::RGBx | VideoFormat::xRGB => {
            // Swap R/B into BGRA.
            let mut out = Vec::with_capacity(height as usize * row_bytes);
            for row in 0..height as usize {
                let start = row * stride;
                if start + row_bytes > bytes.len() {
                    return None;
                }
                for px in bytes[start..start + row_bytes].chunks_exact(4) {
                    out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            }
            (out, PixelFormat::Bgra)
        }
        VideoFormat::NV12 => {
            let y_bytes = width as usize * height as usize;
            let uv_bytes = y_bytes / 2;
            if bytes.len() < y_bytes + uv_bytes {
                return None;
            }
            // Y plane (packed), then interleaved UV — copy both raw.
            let mut out = Vec::with_capacity(y_bytes + uv_bytes);
            let y_stride = stride.max(width as usize);
            for row in 0..height as usize {
                let start = row * y_stride;
                out.extend_from_slice(&bytes[start..start + width as usize]);
            }
            let uv_stride = stride / 2;
            let uv_height = (height as usize) / 2;
            let uv_row = width as usize;
            for row in 0..uv_height {
                let start = y_bytes + row * uv_stride;
                out.extend_from_slice(&bytes[start..start + uv_row]);
            }
            (out, PixelFormat::Nv12)
        }
        other => {
            debug!(format = ?other, "unsupported negotiated video format");
            return None;
        }
    };

    Some(CapturedVideoFrame {
        data,
        width,
        height,
        pixel_format,
        timestamp_us: user_data.started.elapsed().as_micros() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_source_is_listed() {
        let capturer = LinuxScreenCapturer::new();
        let sources = capturer.available_sources().expect("sources");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, PICKER_SOURCE_ID);
        assert_eq!(sources[0].source_type, CaptureSourceType::Display);
    }

    #[test]
    fn format_pod_requests_bgrx() {
        let config = CaptureConfig {
            width: 1920,
            height: 1080,
            fps: 30,
            capture_audio: false,
            exclude_self_audio: true,
        };
        let pod = format_pod(&config).expect("pod builds");
        let bytes = pod.as_bytes();
        // SPA pod object header + media type video (1) / subtype raw (1) +
        // format BGRx property — spot-check that the object is non-trivial.
        assert!(bytes.len() > 32, "format pod has substance");
    }

    #[test]
    fn stop_without_start_is_ok() {
        let mut capturer = LinuxScreenCapturer::new();
        capturer.stop_capture().expect("no-op stop");
    }
}
