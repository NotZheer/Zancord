//! Audio processing (Phase 1C.5): 80Hz Butterworth high-pass filter + noise
//! gate + level metering, ported from the PWA's Web Audio pipeline.
//!
//! All processing runs on the audio worker thread, never inside an RT callback.

/// Second-order Butterworth high-pass biquad (RBJ cookbook).
#[derive(Debug, Clone)]
pub struct HighPassFilter {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
    enabled: bool,
}

impl HighPassFilter {
    /// New HPF with the given cutoff at `sample_rate`.
    pub fn new(cutoff_hz: f32, sample_rate: u32) -> Self {
        let mut filter = Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
            enabled: true,
        };
        filter.set_cutoff(cutoff_hz, sample_rate);
        filter
    }

    /// Recompute coefficients for a new cutoff (2nd-order Butterworth HPF).
    pub fn set_cutoff(&mut self, cutoff_hz: f32, sample_rate: u32) {
        let q = std::f32::consts::FRAC_1_SQRT_2; // Butterworth Q
        let w0 = 2.0 * std::f32::consts::PI * cutoff_hz / sample_rate.max(1) as f32;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        let k = (1.0 + cos_w0) / 2.0;
        self.b0 = k / a0;
        self.b1 = -2.0 * k / a0;
        self.b2 = k / a0;
        self.a1 = -2.0 * cos_w0 / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    /// Filter one sample; returns the filtered sample.
    pub fn process(&mut self, sample: f32) -> f32 {
        if !self.enabled {
            return sample;
        }
        let y = self.b0 * sample + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = sample;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    /// Filter a block of samples in place.
    pub fn process_block(&mut self, block: &mut [f32]) {
        for sample in block.iter_mut() {
            *sample = self.process(*sample);
        }
    }

    /// Bypass the filter (pass-through) when disabled.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Whether the filter is active.
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

/// Default noise gate threshold (dBFS).
pub const DEFAULT_THRESHOLD_DB: f32 = -45.0;
/// User-adjustable threshold range, inclusive.
pub const MIN_THRESHOLD_DB: f32 = -70.0;
pub const MAX_THRESHOLD_DB: f32 = -10.0;
/// Gate open attack time.
pub const ATTACK_MS: f32 = 1.0;
/// Gate close release time.
pub const RELEASE_MS: f32 = 50.0;

/// RMS noise gate: below the threshold the gain ramps to 0 over the release
/// time; above it the gain ramps to 1 over the attack time.
#[derive(Debug, Clone)]
pub struct NoiseGate {
    enabled: bool,
    threshold_db: f32,
    attack_step: f32,
    release_step: f32,
    gain: f32,
}

impl NoiseGate {
    /// New gate with `threshold_db` (clamped to `MIN..=MAX`) at `sample_rate`.
    pub fn new(threshold_db: f32, sample_rate: u32) -> Self {
        let fs = sample_rate.max(1) as f32;
        Self {
            enabled: true,
            threshold_db: threshold_db.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB),
            attack_step: 1.0 / (ATTACK_MS * 0.001 * fs),
            release_step: 1.0 / (RELEASE_MS * 0.001 * fs),
            gain: 1.0,
        }
    }

    /// Set the threshold in dBFS (clamped to `MIN..=MAX`).
    pub fn set_threshold_db(&mut self, db: f32) {
        self.threshold_db = db.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB);
    }

    /// Current threshold in dBFS.
    pub fn threshold_db(&self) -> f32 {
        self.threshold_db
    }

    /// Pass audio through untouched when disabled.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Whether the gate is active.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Current smoothed gain: 0.0 (closed) ..= 1.0 (open).
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Apply the gate to one frame (e.g. 960 samples) in place.
    pub fn process_frame(&mut self, frame: &mut [f32]) {
        let target = if self.enabled && Self::rms_db(frame) >= self.threshold_db {
            1.0
        } else {
            0.0
        };
        for sample in frame.iter_mut() {
            if self.gain < target {
                self.gain = (self.gain + self.attack_step).min(target);
            } else if self.gain > target {
                self.gain = (self.gain - self.release_step).max(target);
            }
            *sample *= self.gain;
        }
    }

    /// RMS level of a frame in dBFS (`-inf` for digital silence).
    pub fn rms_db(frame: &[f32]) -> f32 {
        if frame.is_empty() {
            return f32::NEG_INFINITY;
        }
        let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
        let rms = (sum_sq / frame.len() as f32).sqrt();
        20.0 * (rms + 1e-8).log10()
    }
}

/// Normalized level reading, both values in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelReading {
    pub peak: f32,
    pub rms: f32,
}

/// Peak + RMS metering with throttled emission (every `emit_interval_ms` of
/// samples, so behavior is deterministic regardless of wall-clock jitter).
#[derive(Debug)]
pub struct LevelMeter {
    peak: f32,
    rms_sum_sq: f32,
    rms_count: usize,
    samples_per_emit: usize,
    samples_since_emit: usize,
}

impl Default for LevelMeter {
    fn default() -> Self {
        Self::new(crate::codec::SAMPLE_RATE, 50)
    }
}

impl LevelMeter {
    /// New meter emitting a reading every `emit_interval_ms` at `sample_rate`.
    pub fn new(sample_rate: u32, emit_interval_ms: u64) -> Self {
        Self {
            peak: 0.0,
            rms_sum_sq: 0.0,
            rms_count: 0,
            samples_per_emit: (sample_rate.max(1) as u64 * emit_interval_ms.max(1) / 1000) as usize,
            samples_since_emit: 0,
        }
    }

    /// Feed one processed frame; returns a reading when the emit interval
    /// elapses. RMS is normalized so `-60 dBFS → 0.0` and `0 dBFS → 1.0`.
    pub fn process_frame(&mut self, frame: &[f32]) -> Option<LevelReading> {
        for &sample in frame {
            let magnitude = sample.abs();
            if magnitude > self.peak {
                self.peak = magnitude;
            }
            self.rms_sum_sq += sample * sample;
        }
        self.rms_count += frame.len();
        self.samples_since_emit += frame.len();
        if self.samples_since_emit < self.samples_per_emit {
            return None;
        }
        self.samples_since_emit = 0;
        let rms = (self.rms_sum_sq / self.rms_count.max(1) as f32).sqrt();
        let rms_norm = ((20.0 * (rms + 1e-8).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
        let reading = LevelReading {
            peak: self.peak.clamp(0.0, 1.0),
            rms: rms_norm,
        };
        self.peak = 0.0;
        self.rms_sum_sq = 0.0;
        self.rms_count = 0;
        Some(reading)
    }
}
