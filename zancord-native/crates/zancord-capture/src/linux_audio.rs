//! Linux system audio capture via the PipeWire monitor of the default sink
//! (Phase 3B.2).
//!
//! The XDG ScreenCast portal has no audio, so screen-share audio on Linux
//! comes from a separate PipeWire capture stream attached to the default
//! sink's monitor port (`STREAM_CAPTURE_SINK`). Delivers interleaved f32 PCM
//! as `CapturedAudioFrame` (48 kHz stereo on typical desktop graphs).

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use pipewire as pw;
use pipewire::properties::properties;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::param::ParamType;
use pw::spa::pod::{serialize::PodSerializer, Object, Pod, Value};
use pw::spa::utils::SpaTypes;
use tracing::{debug, info, warn};

use crate::traits::CapturedAudioFrame;

/// Per-stream state shared with the PipeWire process callback.
struct AudioUserData {
    format: AudioInfoRaw,
    audio_tx: Sender<CapturedAudioFrame>,
    started: Instant,
}

/// PipeWire monitor capturer for system audio. The process callback runs on
/// the thread loop's internal thread; samples are forwarded over a channel.
pub struct LinuxSystemAudioCapturer {
    audio_tx: Sender<CapturedAudioFrame>,
    audio_rx: Receiver<CapturedAudioFrame>,
    stream_error: Arc<Mutex<Option<String>>>,
    /// Leaked once per start (opaque ZST — no memory cost).
    thread_loop: Option<&'static pw::thread_loop::ThreadLoop>,
    /// Drop order matters: listener → stream → core → context → loop.
    _listener: Option<pw::stream::StreamListener<AudioUserData>>,
    stream: Option<pw::stream::StreamBox<'static>>,
    core: Option<pw::core::CoreBox<'static>>,
    context: Option<pw::context::ContextBox<'static>>,
}

impl LinuxSystemAudioCapturer {
    pub fn new() -> Self {
        pw::init();
        let (audio_tx, audio_rx) = mpsc::channel();
        Self {
            audio_tx,
            audio_rx,
            stream_error: Arc::new(Mutex::new(None)),
            thread_loop: None,
            _listener: None,
            stream: None,
            core: None,
            context: None,
        }
    }

    /// Starts capturing the default sink's monitor (autoconnect).
    pub fn start(&mut self) -> Result<()> {
        if self.thread_loop.is_some() {
            self.stop()?;
        }

        let thread_loop: &'static pw::thread_loop::ThreadLoop =
            Box::leak(Box::new(pw::thread_loop::ThreadLoop::new(None)?));
        thread_loop.start();

        let context = pw::context::ContextBox::new(thread_loop.loop_(), None)
            .context("pw_context_new failed")?;
        let core = context.connect(None).context("pw_context_connect failed")?;
        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::STREAM_CAPTURE_SINK => "true",
        };
        let stream = pw::stream::StreamBox::new(&core, "zancord-screen-audio", props)
            .context("pw_stream_new failed")?;

        let user_data = AudioUserData {
            format: AudioInfoRaw::new(),
            audio_tx: self.audio_tx.clone(),
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
                    warn!(error = %err, "failed to parse negotiated audio format");
                }
            })
            .process(|stream, user_data| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(frame) = buffer_to_audio_frame(&mut buffer, user_data) else {
                    return;
                };
                if user_data.audio_tx.send(frame).is_err() {
                    *user_data.stream_error.lock().expect("stream_error lock") =
                        Some("audio channel closed".to_owned());
                }
            })
            .register()
            .context("failed to register stream listener")?;

        // Request float32; rate/channels stay open so the graph picks its
        // native values (the reported format arrives via param_changed).
        let mut audio_info = AudioInfoRaw::new();
        audio_info.set_format(AudioFormat::F32LE);
        let obj = Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        };
        let values =
            PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
                .context("failed to serialize audio format pod")?
                .0
                .into_inner();
        let mut params = [Pod::from_bytes(&values).context("failed to parse audio format pod")?];

        stream
            .connect(
                pw::spa::utils::Direction::Input,
                None, // autoconnect to the default sink monitor
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .context("pw_stream_connect failed")?;

        self.thread_loop = Some(thread_loop);
        self.context = Some(context);
        self.core = Some(core);
        self.stream = Some(stream);
        self._listener = Some(listener);
        info!("system audio capture started (default sink monitor)");
        Ok(())
    }

    /// Stops the capture stream. Errors if the stream already died on its own.
    pub fn stop(&mut self) -> Result<()> {
        if let Some(thread_loop) = self.thread_loop.take() {
            thread_loop.stop();
        }
        self._listener = None;
        self.stream = None;
        self.core = None;
        self.context = None;
        if let Some(err) = self.stream_error.lock().expect("stream_error lock").take() {
            anyhow::bail!("system audio capture ended with an error: {err}");
        }
        info!("system audio capture stopped");
        Ok(())
    }

    /// Receiver for captured interleaved f32 PCM samples.
    pub fn audio_sample_rx(&self) -> &Receiver<CapturedAudioFrame> {
        &self.audio_rx
    }
}

impl Default for LinuxSystemAudioCapturer {
    fn default() -> Self {
        Self::new()
    }
}

/// Copies one captured buffer out as a `CapturedAudioFrame` (interleaved f32
/// PCM, whatever rate/channels the graph negotiated).
fn buffer_to_audio_frame(
    buffer: &mut pw::buffer::Buffer<'_>,
    user_data: &mut AudioUserData,
) -> Option<CapturedAudioFrame> {
    let datas = buffer.datas_mut();
    let data = datas.first_mut()?;
    let slice = data.data()?;
    let size = data.chunk().size() as usize;
    if size == 0 {
        return None;
    }
    let bytes = &slice[..size.min(slice.len())];
    let mut pcm = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        pcm.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    if pcm.is_empty() {
        return None;
    }
    let channels = user_data.format.channels().max(1) as u16;
    let sample_rate = user_data.format.rate().max(1);
    debug!(
        samples = pcm.len(),
        channels, sample_rate, "system audio frame"
    );
    Some(CapturedAudioFrame {
        pcm_data: pcm,
        sample_rate,
        channels,
        timestamp_us: user_data.started.elapsed().as_micros() as u64,
    })
}
