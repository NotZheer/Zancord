//! Audio pipeline orchestrator (Phase 1C.6): dedicated std::thread, 20ms tick
//! loop. capture → resample → HPF → gate → meter → Opus encode → transport;
//! transport → Opus decode → mix → resample → playback.
//!
//! REAL-TIME SAFETY: cpal callbacks only touch lock-free `rtrb` rings (see
//! `capture.rs` / `playback.rs`). This pipeline runs on a plain `std::thread`
//! and may allocate freely — it must never run inside an RT callback.
//!
//! Note: `cpal::Stream` is deliberately `!Send` in cpal 0.15, so hardware is
//! opened inside the audio thread (`AudioPipeline::spawn`); the pipeline is
//! constructed with injected rings (`with_io`) for tests.

use std::collections::HashMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{self, Receiver, Sender};
use zancord_protocol::{AudioProcessingConfig, EncodedAudioFrame, PeerId};

use crate::capture::MicCapture;
use crate::codec::{
    OpusDecoder, OpusEncoder, FRAME_SIZE, FRAME_SIZE_STEREO, MAX_PACKET_BYTES, SAMPLE_RATE,
};
use crate::devices;
use crate::error::Result;
use crate::playback::Playback;
use crate::processor::{HighPassFilter, LevelMeter, NoiseGate};
use crate::resampler::{CaptureResampler, PlaybackResampler};

/// Nominal tick duration: one Opus frame (20 ms).
pub const TICK_INTERVAL: Duration = Duration::from_millis(20);
/// Control command channel capacity (UI/orchestrator → audio thread).
pub const CONTROL_CAPACITY: usize = 32;
/// Transport frame queue capacity (frames to/from the WebRTC layer).
pub const FRAME_QUEUE_CAPACITY: usize = 256;

/// Configuration for the audio pipeline.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub processing: AudioProcessingConfig,
    /// Opus bitrate in bits/second (default 32 kbps).
    pub opus_bitrate: i32,
    /// In-band FEC on the encoder (default on).
    pub opus_fec: bool,
    /// Max consecutive missing frames concealed per gap before giving up.
    pub max_plc_frames: usize,
    pub capture_ring_capacity: usize,
    pub playback_ring_capacity: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            processing: AudioProcessingConfig::default(),
            opus_bitrate: 32_000,
            opus_fec: true,
            max_plc_frames: 4,
            capture_ring_capacity: 48_000,
            playback_ring_capacity: 48_000,
        }
    }
}

/// Commands from other threads (UI/orchestrator), applied on the audio thread.
#[derive(Debug, Clone)]
pub enum AudioControl {
    SetPeerVolume { peer: PeerId, volume: f32 },
    RemovePeer { peer: PeerId },
    SetDeafened { deafened: bool },
    SetProcessing(AudioProcessingConfig),
    Shutdown,
}

/// Per-tick accounting, useful for diagnostics and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickSummary {
    /// Opus frames encoded and sent to the transport.
    pub sent_frames: u64,
    /// Frames received from the transport.
    pub received_frames: u64,
    /// Frames decoded (excluding PLC).
    pub decoded_frames: u64,
    /// Screen-audio (stereo) frames decoded and downmixed.
    pub screen_frames: u64,
    /// Missing frames concealed with PLC.
    pub plc_frames: u64,
    /// Frames dropped as stale/duplicate (sequence ≤ last seen).
    pub dropped_peer_frames: u64,
    /// Frames dropped because the transport queue was full/closed.
    pub tx_dropped: u64,
    /// Mono samples pushed to the speaker ring.
    pub playback_pushed: u64,
}

/// Which local audio source an incoming frame belongs to. Mic frames decode
/// mono; screen-audio frames decode stereo and are downmixed into the mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IncomingAudioKind {
    Mic,
    ScreenAudio,
}

/// The audio pipeline. Owned exclusively by the audio worker thread.
pub struct AudioPipeline {
    capture: Option<MicCapture>,
    playback: Option<Playback>,
    capture_resampler: Option<CaptureResampler>,
    playback_resampler: Option<PlaybackResampler>,
    hpf: HighPassFilter,
    gate: NoiseGate,
    meter: LevelMeter,
    encoder: OpusEncoder,
    decoder: OpusDecoder,
    screen_decoder: OpusDecoder,
    tx: Sender<EncodedAudioFrame>,
    rx: Receiver<(PeerId, IncomingAudioKind, EncodedAudioFrame)>,
    control_rx: Receiver<AudioControl>,
    config: PipelineConfig,
    sequence: u64,
    started_at: Instant,
    last_seq: HashMap<(PeerId, IncomingAudioKind), u64>,
    stopped: bool,
    // Scratch buffers (worker thread only).
    pcm_in: Vec<i16>,
    packet_out: Vec<u8>,
    decode_out: Vec<i16>,
    screen_decode_out: Vec<i16>,
    frame_buf: Vec<f32>,
    mix_buf: Vec<f32>,
    ring_scratch: [f32; 4096],
}

impl AudioPipeline {
    /// Build a pipeline with injected capture/playback halves (no hardware).
    ///
    /// Tests pass `MicCapture::from_ring` / `Playback::from_parts`; the real
    /// entry point is [`AudioPipeline::spawn`], which opens hardware inside
    /// the audio thread (cpal streams are `!Send`).
    pub fn with_io(
        capture: Option<MicCapture>,
        playback: Option<Playback>,
        config: PipelineConfig,
        tx: Sender<EncodedAudioFrame>,
        rx: Receiver<(PeerId, IncomingAudioKind, EncodedAudioFrame)>,
        control_rx: Receiver<AudioControl>,
    ) -> Result<Self> {
        let capture_resampler = match &capture {
            Some(capture) => Some(CaptureResampler::new(
                capture.sample_rate(),
                usize::from(capture.channels()),
            )?),
            None => None,
        };
        let playback_resampler = match &playback {
            Some(playback) => Some(PlaybackResampler::new(playback.sample_rate())?),
            None => None,
        };
        Ok(Self {
            capture,
            playback,
            capture_resampler,
            playback_resampler,
            hpf: HighPassFilter::new(config.processing.hpf_cutoff_hz, SAMPLE_RATE),
            gate: NoiseGate::new(config.processing.noise_gate_threshold_db, SAMPLE_RATE),
            meter: LevelMeter::new(SAMPLE_RATE, 50),
            encoder: OpusEncoder::new(config.opus_bitrate)?,
            decoder: OpusDecoder::new()?,
            screen_decoder: OpusDecoder::new_stereo()?,
            tx,
            rx,
            control_rx,
            config,
            sequence: 0,
            started_at: Instant::now(),
            last_seq: HashMap::new(),
            stopped: false,
            pcm_in: vec![0; FRAME_SIZE],
            packet_out: Vec::with_capacity(MAX_PACKET_BYTES),
            decode_out: vec![0; FRAME_SIZE],
            screen_decode_out: vec![0; FRAME_SIZE_STEREO],
            frame_buf: vec![0.0; FRAME_SIZE],
            mix_buf: vec![0.0; FRAME_SIZE],
            ring_scratch: [0.0; 4096],
        })
    }

    /// Open hardware (inside the audio thread — `cpal::Stream` is `!Send`) and
    /// run the pipeline on a dedicated `std::thread` until the transport drops
    /// the receive channel or an `AudioControl::Shutdown` arrives.
    ///
    /// Device ids come from `devices::list_input_devices()` /
    /// `list_output_devices()`; `None` disables that direction.
    pub fn spawn(
        config: PipelineConfig,
        input_device_id: Option<String>,
        output_device_id: Option<String>,
        tx: Sender<EncodedAudioFrame>,
        rx: Receiver<(PeerId, IncomingAudioKind, EncodedAudioFrame)>,
        control_rx: Receiver<AudioControl>,
    ) -> Result<JoinHandle<()>> {
        let handle = std::thread::Builder::new()
            .name("zancord-audio-pipeline".to_string())
            .spawn(move || {
                let result = Self::open_and_run(
                    config,
                    input_device_id,
                    output_device_id,
                    tx,
                    rx,
                    control_rx,
                );
                if let Err(error) = result {
                    tracing::error!(target: "zancord_audio", %error, "audio pipeline failed to start");
                }
            })?;
        Ok(handle)
    }

    fn open_and_run(
        config: PipelineConfig,
        input_device_id: Option<String>,
        output_device_id: Option<String>,
        tx: Sender<EncodedAudioFrame>,
        rx: Receiver<(PeerId, IncomingAudioKind, EncodedAudioFrame)>,
        control_rx: Receiver<AudioControl>,
    ) -> Result<()> {
        let mut devices = devices::DeviceManager::new();
        let capture = match input_device_id {
            Some(id) => {
                devices.set_input_device(&id)?;
                let device = devices.input_device()?;
                let capture = MicCapture::open(&device, config.capture_ring_capacity)?;
                tracing::info!(
                    target: "zancord_audio",
                    device = %id,
                    rate = capture.sample_rate(),
                    channels = capture.channels(),
                    "mic capture open"
                );
                Some(capture)
            }
            None => None,
        };
        let playback = match output_device_id {
            Some(id) => {
                devices.set_output_device(&id)?;
                let device = devices.output_device()?;
                let playback = Playback::open(&device, config.playback_ring_capacity)?;
                tracing::info!(
                    target: "zancord_audio",
                    device = %id,
                    rate = playback.sample_rate(),
                    channels = playback.channels(),
                    "playback open"
                );
                Some(playback)
            }
            None => None,
        };
        let mut pipeline = Self::with_io(capture, playback, config, tx, rx, control_rx)?;
        pipeline.run_loop();
        Ok(())
    }

    fn run_loop(&mut self) {
        // Per-second activity accounting so a voice call can be verified from
        // logs alone: `sent` proves mic -> Opus -> network; `received`/`decoded`
        // prove network -> Opus -> speakers.
        let mut last_report = Instant::now();
        let mut sent = 0u64;
        let mut received = 0u64;
        let mut decoded = 0u64;
        let mut screen = 0u64;
        let mut plc = 0u64;
        let mut dropped = 0u64;

        while !self.stopped {
            let started = Instant::now();
            match self.tick() {
                Ok(summary) => {
                    sent += summary.sent_frames;
                    received += summary.received_frames;
                    decoded += summary.decoded_frames;
                    screen += summary.screen_frames;
                    plc += summary.plc_frames;
                    dropped += summary.dropped_peer_frames + summary.tx_dropped;
                }
                Err(error) => {
                    tracing::error!(target: "zancord_audio", %error, "audio tick failed");
                }
            }
            if last_report.elapsed() >= Duration::from_secs(1) {
                let (overflow, ring_slots) = match &mut self.capture {
                    Some(c) => (c.overflow_count(), c.consumer().slots()),
                    None => (0, 0),
                };
                let (input_buffered, pending) = match &self.capture_resampler {
                    Some(r) => (r.input_buffered(), r.pending_len()),
                    None => (0, 0),
                };
                tracing::info!(
                    target: "zancord_audio",
                    sent,
                    received,
                    decoded,
                    screen,
                    plc,
                    dropped,
                    overflow,
                    ring_slots,
                    input_buffered,
                    pending,
                    "audio activity (last 1s): sent=mic->net received=net->decoder decoded=decoder->speakers"
                );
                sent = 0;
                received = 0;
                decoded = 0;
                screen = 0;
                plc = 0;
                dropped = 0;
                last_report = Instant::now();
            }
            if self.rx.is_closed() {
                break;
            }
            let elapsed = started.elapsed();
            if elapsed < TICK_INTERVAL {
                std::thread::sleep(TICK_INTERVAL - elapsed);
            }
        }
        tracing::info!(target: "zancord_audio", "audio pipeline stopped");
    }

    /// One 20 ms slice of work: control commands, receive path, capture path.
    ///
    /// Public so tests can drive the pipeline synchronously.
    pub fn tick(&mut self) -> Result<TickSummary> {
        let mut summary = TickSummary::default();
        self.drain_control();
        self.receive_frames(&mut summary)?;
        self.capture_frames(&mut summary)?;
        Ok(summary)
    }

    /// Whether a shutdown has been requested (or tick stopped the loop).
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    fn drain_control(&mut self) {
        while let Ok(command) = self.control_rx.try_recv() {
            match command {
                AudioControl::SetPeerVolume { peer, volume } => {
                    if let Some(playback) = &mut self.playback {
                        playback.mixer_mut().set_peer_volume(peer, volume);
                    }
                }
                AudioControl::RemovePeer { peer } => {
                    self.last_seq.retain(|(p, _), _| p != &peer);
                    if let Some(playback) = &mut self.playback {
                        playback.mixer_mut().remove_peer(&peer);
                    }
                }
                AudioControl::SetDeafened { deafened } => {
                    if let Some(playback) = &mut self.playback {
                        playback.mixer_mut().set_deafened(deafened);
                    }
                }
                AudioControl::SetProcessing(config) => {
                    self.hpf.set_enabled(config.hpf_enabled);
                    self.hpf.set_cutoff(config.hpf_cutoff_hz, SAMPLE_RATE);
                    self.gate.set_enabled(config.noise_gate_enabled);
                    self.gate.set_threshold_db(config.noise_gate_threshold_db);
                    self.config.processing = config;
                }
                AudioControl::Shutdown => self.stopped = true,
            }
        }
    }

    // --- Receive path: transport → decode (+PLC) → mix → resample → ring ---

    fn receive_frames(&mut self, summary: &mut TickSummary) -> Result<()> {
        while let Ok((peer, kind, frame)) = self.rx.try_recv() {
            summary.received_frames += 1;
            self.ingest(peer, kind, frame, summary)?;
        }
        Ok(())
    }

    fn ingest(
        &mut self,
        peer: PeerId,
        kind: IncomingAudioKind,
        frame: EncodedAudioFrame,
        summary: &mut TickSummary,
    ) -> Result<()> {
        let key = (peer.clone(), kind);
        // Stale or duplicate (reordered delivery): drop, never re-decode.
        if self
            .last_seq
            .get(&key)
            .is_some_and(|&last| frame.sequence <= last)
        {
            summary.dropped_peer_frames += 1;
            return Ok(());
        }
        // Conceal any missing frames with PLC before decoding the real one.
        if let Some(&last) = self.last_seq.get(&key) {
            let gap = frame.sequence - last - 1;
            let concealed = gap.min(self.config.max_plc_frames as u64);
            for _ in 0..concealed {
                self.decode_and_mix(&peer, kind, None, summary)?;
                self.flush_mix(summary)?;
            }
        }
        self.last_seq.insert(key, frame.sequence);
        self.decode_and_mix(&peer, kind, Some(&frame.data), summary)?;
        self.flush_mix(summary)?;
        Ok(())
    }

    /// Decodes a frame (or conceals one) and adds it to the mix. Stereo
    /// screen-audio frames are downmixed to mono before joining the mix.
    fn decode_and_mix(
        &mut self,
        peer: &PeerId,
        kind: IncomingAudioKind,
        packet: Option<&[u8]>,
        summary: &mut TickSummary,
    ) -> Result<()> {
        let (written, samples) = match kind {
            IncomingAudioKind::Mic => {
                let written = self.decoder.decode(packet, &mut self.decode_out)?;
                (written, &self.decode_out)
            }
            IncomingAudioKind::ScreenAudio => {
                let written = self
                    .screen_decoder
                    .decode(packet, &mut self.screen_decode_out)?;
                (written, &self.screen_decode_out)
            }
        };
        let volume = self
            .playback
            .as_ref()
            .map(|playback| playback.mixer().peer_volume(peer))
            .unwrap_or(1.0);
        match kind {
            IncomingAudioKind::Mic => {
                for (slot, &sample) in self.mix_buf.iter_mut().zip(samples.iter().take(written)) {
                    *slot += f32::from(sample) / 32768.0 * volume;
                }
            }
            IncomingAudioKind::ScreenAudio => {
                // Stereo → mono downmix (average L/R). `written` is the
                // per-channel sample count (960 for a 20 ms frame), i.e. the
                // number of L/R pairs to consume.
                for (slot, pair) in self
                    .mix_buf
                    .iter_mut()
                    .zip(samples.chunks_exact(2).take(written))
                {
                    let l = f32::from(pair[0]);
                    let r = f32::from(pair[1]);
                    *slot += ((l + r) / 2.0) / 32768.0 * volume;
                }
            }
        }
        if packet.is_none() {
            summary.plc_frames += 1;
        } else if matches!(kind, IncomingAudioKind::ScreenAudio) {
            summary.screen_frames += 1;
        } else {
            summary.decoded_frames += 1;
        }
        Ok(())
    }

    fn flush_mix(&mut self, summary: &mut TickSummary) -> Result<()> {
        let Some(playback) = &mut self.playback else {
            self.mix_buf.fill(0.0);
            return Ok(());
        };
        playback.mixer().apply_global_gain(&mut self.mix_buf);
        let resampler = self
            .playback_resampler
            .as_mut()
            .expect("playback without playback resampler");
        let out = resampler.process(&self.mix_buf)?;
        let pushed = playback.push(out);
        summary.playback_pushed += pushed as u64;
        self.mix_buf.fill(0.0);
        Ok(())
    }

    // --- Capture path: ring → resample → HPF → gate → meter → Opus → send ---

    fn capture_frames(&mut self, summary: &mut TickSummary) -> Result<()> {
        let Some(capture) = &mut self.capture else {
            return Ok(());
        };
        let consumer = capture.consumer();

        // Drain everything currently in the mic ring into the resampler.
        let mut filled = 0usize;
        while let Ok(sample) = consumer.pop() {
            self.ring_scratch[filled] = sample;
            filled += 1;
            if filled == self.ring_scratch.len() {
                self.capture_resampler
                    .as_mut()
                    .expect("capture without capture resampler")
                    .push(&self.ring_scratch)?;
                filled = 0;
            }
        }
        if filled > 0 {
            self.capture_resampler
                .as_mut()
                .expect("capture without capture resampler")
                .push(&self.ring_scratch[..filled])?;
        }

        // Encode every complete 48 kHz frame now available.
        loop {
            let resampler = self
                .capture_resampler
                .as_mut()
                .expect("capture without capture resampler");
            if !resampler.take_frame(&mut self.frame_buf) {
                break;
            }
            if self.config.processing.hpf_enabled {
                self.hpf.process_block(&mut self.frame_buf);
            }
            if self.config.processing.noise_gate_enabled {
                self.gate.process_frame(&mut self.frame_buf);
            }
            if let Some(reading) = self.meter.process_frame(&self.frame_buf) {
                tracing::trace!(
                    target: "zancord_audio",
                    peak = reading.peak,
                    rms = reading.rms,
                    "mic level"
                );
            }
            for (slot, &sample) in self.pcm_in.iter_mut().zip(self.frame_buf.iter()) {
                *slot = (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            }
            self.encoder
                .encode_into(&self.pcm_in, &mut self.packet_out)?;
            let frame = EncodedAudioFrame {
                data: self.packet_out.clone(),
                sequence: self.sequence,
                timestamp_ms: self.started_at.elapsed().as_millis() as u64,
            };
            self.sequence += 1;
            match self.tx.try_send(frame) {
                Ok(()) => summary.sent_frames += 1,
                // Full/closed transport: drop the frame — fresh audio matters
                // more than complete audio.
                Err(mpsc::error::TrySendError::Full(_))
                | Err(mpsc::error::TrySendError::Closed(_)) => summary.tx_dropped += 1,
            }
        }
        Ok(())
    }
}
