//! Opus codec wrapper (Phase 1C.4 / 3B.3): 48 kHz mono VoIP encoder/decoder
//! for the mic, plus a 48 kHz stereo Music encoder/decoder for screen-share
//! audio. 960-sample (20 ms) mono frames / 1920-sample (20 ms) interleaved
//! stereo frames, in-band FEC, PLC on missing packets.

use opus::{
    Application, Bitrate, Channels, Decoder as OpusDecoderInner, Encoder as OpusEncoderInner,
};

use crate::error::{AudioError, Result};

/// Opus sample rate (fixed by the spec for 20 ms frames).
pub const SAMPLE_RATE: u32 = 48_000;
/// Samples per 20 ms mono frame at [`SAMPLE_RATE`].
pub const FRAME_SIZE: usize = 960;
/// Samples per 20 ms interleaved stereo frame (2 × [`FRAME_SIZE`]).
pub const FRAME_SIZE_STEREO: usize = FRAME_SIZE * 2;
/// Largest possible encoded Opus frame at 48 kHz.
pub const MAX_PACKET_BYTES: usize = 1275;
/// Default bitrate: 32 kbps (configurable).
pub const DEFAULT_BITRATE: i32 = 32_000;
/// Screen-audio bitrate (stereo Music): 64 kbps per the plan.
pub const SCREEN_AUDIO_BITRATE: i32 = 64_000;
/// Packet-loss estimate fed to the encoder so in-band FEC is actually produced.
const PACKET_LOSS_PERCENT: i32 = 10;

/// Opus encoder: 48 kHz, mono VoIP (mic) or stereo Music (screen audio).
pub struct OpusEncoder {
    inner: OpusEncoderInner,
    bitrate: i32,
    fec: bool,
    frame_samples: usize,
}

impl OpusEncoder {
    /// New mono mic encoder at `bitrate` bits/second with in-band FEC.
    pub fn new(bitrate: i32) -> Result<Self> {
        Self::new_with(bitrate, Channels::Mono, Application::Voip, FRAME_SIZE)
    }

    /// New stereo screen-audio encoder: Music application (per the plan),
    /// interleaved input, 20 ms frames.
    pub fn new_stereo(bitrate: i32) -> Result<Self> {
        Self::new_with(
            bitrate,
            Channels::Stereo,
            Application::Audio,
            FRAME_SIZE_STEREO,
        )
    }

    fn new_with(
        bitrate: i32,
        channels: Channels,
        application: Application,
        frame_samples: usize,
    ) -> Result<Self> {
        let mut inner = OpusEncoderInner::new(SAMPLE_RATE, channels, application)?;
        inner.set_bitrate(Bitrate::Bits(bitrate))?;
        inner.set_inband_fec(true)?;
        inner.set_packet_loss_perc(PACKET_LOSS_PERCENT)?;
        Ok(Self {
            inner,
            bitrate,
            fec: true,
            frame_samples,
        })
    }

    /// Encode one `frame_samples`-sample (mono) or interleaved (stereo) `i16`
    /// frame into a fresh packet.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        let mut out = vec![0u8; MAX_PACKET_BYTES];
        let n = self.encode_into(pcm, &mut out)?;
        out.truncate(n);
        Ok(out)
    }

    /// Encode into a caller-owned buffer (reused across calls); returns the
    /// packet length in bytes.
    pub fn encode_into(&mut self, pcm: &[i16], out: &mut Vec<u8>) -> Result<usize> {
        if pcm.len() != self.frame_samples {
            return Err(AudioError::Config(format!(
                "opus encoder expects {} samples, got {}",
                self.frame_samples,
                pcm.len()
            )));
        }
        out.resize(MAX_PACKET_BYTES, 0);
        let n = self.inner.encode(pcm, out)?;
        out.truncate(n);
        Ok(n)
    }

    /// Change the bitrate (bits/second).
    pub fn set_bitrate(&mut self, bitrate: i32) -> Result<()> {
        self.inner.set_bitrate(Bitrate::Bits(bitrate))?;
        self.bitrate = bitrate;
        Ok(())
    }

    /// Current bitrate in bits/second.
    pub fn bitrate(&self) -> i32 {
        self.bitrate
    }

    /// Toggle in-band FEC.
    pub fn set_fec(&mut self, enabled: bool) -> Result<()> {
        self.inner.set_inband_fec(enabled)?;
        self.fec = enabled;
        Ok(())
    }

    /// Whether in-band FEC is enabled.
    pub fn fec(&self) -> bool {
        self.fec
    }
}

/// Opus decoder: 48 kHz mono (mic) or stereo (screen audio), PLC via
/// empty-packet decode.
pub struct OpusDecoder {
    inner: OpusDecoderInner,
    frame_samples: usize,
}

impl Default for OpusDecoder {
    fn default() -> Self {
        Self::new().expect("opus decoder creation cannot fail")
    }
}

impl OpusDecoder {
    /// New 48 kHz mono decoder (mic frames).
    pub fn new() -> Result<Self> {
        Self::new_with(Channels::Mono, FRAME_SIZE)
    }

    /// New 48 kHz stereo decoder (screen-audio frames).
    pub fn new_stereo() -> Result<Self> {
        Self::new_with(Channels::Stereo, FRAME_SIZE_STEREO)
    }

    fn new_with(channels: Channels, frame_samples: usize) -> Result<Self> {
        Ok(Self {
            inner: OpusDecoderInner::new(SAMPLE_RATE, channels)?,
            frame_samples,
        })
    }

    /// Decode a packet into `out` (`i16`, mono or interleaved stereo, at least
    /// `frame_samples` long). Pass `None` to conceal one lost frame (PLC).
    /// Returns samples written.
    pub fn decode(&mut self, packet: Option<&[u8]>, out: &mut [i16]) -> Result<usize> {
        if out.len() < self.frame_samples {
            return Err(AudioError::Config(format!(
                "opus decoder output must hold {} samples, got {}",
                self.frame_samples,
                out.len()
            )));
        }
        let n = self.inner.decode(packet.unwrap_or(&[]), out, false)?;
        Ok(n)
    }

    /// Decode one frame into a fresh buffer (`None` packet = PLC).
    pub fn decode_frame(&mut self, packet: Option<&[u8]>) -> Result<Vec<i16>> {
        let mut out = vec![0i16; self.frame_samples];
        let n = self.decode(packet, &mut out)?;
        out.truncate(n);
        Ok(out)
    }
}
