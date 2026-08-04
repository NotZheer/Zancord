import { EventBus } from './EventBus';
import { Events } from '../types';

// Map a dBFS threshold to the 0-100 meter scale used by the analyser loop.
// -10 dBFS ≈ level 63, -45 dBFS ≈ level 1 (effectively off).
export function thresholdToLevel(db: number): number {
  return ((Math.pow(10, db / 20) * 255) / 128) * 100;
}

/**
 * Gate decision: below the threshold level the gate closes (gain 0),
 * otherwise it opens (gain 1). Disabled → always open.
 */
export function computeGateTarget(level: number, thresholdDb: number, enabled: boolean): number {
  if (!enabled) return 1;
  return level < thresholdToLevel(thresholdDb) ? 0 : 1;
}

/**
 * P5: the metering loop should only emit to the UI when the rounded level
 * actually moves — otherwise a 60fps rAF loop writes CSS + event-bus churn
 * forever even in complete silence.
 */
export function shouldEmitLevel(prev: number, next: number, minDelta = 1): boolean {
  return Math.abs(next - prev) >= minDelta;
}

export class AudioProcessor {
  private audioCtx: AudioContext | null = null;
  private sourceNode: MediaStreamAudioSourceNode | null = null;
  private hpfNode: BiquadFilterNode | null = null;
  private gateGainNode: GainNode | null = null;
  private analyserNode: AnalyserNode | null = null;
  private destinationNode: MediaStreamAudioDestinationNode | null = null;

  private isEnabled: boolean = true;
  private thresholdDb: number = -45;
  private animFrameId: number | null = null;
  private currentLevel: number = 0;
  private lastGateTarget: number | null = null;
  private eventBus: EventBus;

  constructor(eventBus: EventBus) {
    this.eventBus = eventBus;
  }

  public processStream(rawStream: MediaStream): MediaStream {
    const audioTrack = rawStream.getAudioTracks()[0];
    if (!audioTrack) {
      console.warn('[AUDIO PROCESSOR] No audio track found in raw stream.');
      return rawStream;
    }

    try {
      this.destroy(); // Clean up previous context if any

      const AudioContextClass = window.AudioContext || (window as any).webkitAudioContext;
      this.audioCtx = new AudioContextClass();

      this.sourceNode = this.audioCtx.createMediaStreamSource(new MediaStream([audioTrack]));

      // 1. Highpass filter node (80Hz)
      this.hpfNode = this.audioCtx.createBiquadFilter();
      this.hpfNode.type = 'highpass';
      this.hpfNode.frequency.value = 80;
      this.hpfNode.Q.value = 0.7;

      // 2. REAL noise gate: a GainNode driven by the analyser loop.
      //    (A DynamicsCompressor cannot silence below-threshold signal — it
      //    only squashes it, so fans remain audible. A gain drop to 0 does.)
      this.gateGainNode = this.audioCtx.createGain();
      this.gateGainNode.gain.value = 1;

      // 3. Analyser Node — meters the PRE-gate signal so the gate decision
      //    is stable and the UI ring shows your actual input level.
      this.analyserNode = this.audioCtx.createAnalyser();
      this.analyserNode.fftSize = 256;
      this.analyserNode.smoothingTimeConstant = 0.8;

      // 4. MediaStream Destination
      this.destinationNode = this.audioCtx.createMediaStreamDestination();

      // Connect pipeline
      this.wirePipeline();

      // Start volume metering RAF loop (also drives the gate)
      this.startMetering();

      const processedStream = this.destinationNode.stream;

      // Ensure processed video track is preserved if present
      const videoTrack = rawStream.getVideoTracks()[0];
      if (videoTrack) {
        processedStream.addTrack(videoTrack);
      }

      console.log('[AUDIO PROCESSOR] Web Audio noise gate & HPF pipeline initialized successfully.');
      return processedStream;
    } catch (err) {
      console.error('[AUDIO PROCESSOR] Pipeline initialization failed, falling back to raw stream:', err);
      return rawStream;
    }
  }

  private wirePipeline(): void {
    if (!this.sourceNode || !this.hpfNode || !this.analyserNode || !this.gateGainNode || !this.destinationNode) {
      return;
    }

    // Disconnect all first
    try {
      this.sourceNode.disconnect();
      this.hpfNode.disconnect();
      this.analyserNode.disconnect();
      this.gateGainNode.disconnect();
    } catch (_) {}

    if (this.isEnabled) {
      // Full processing chain: source → HPF → analyser → gate → destination
      this.sourceNode.connect(this.hpfNode);
      this.hpfNode.connect(this.analyserNode);
      this.analyserNode.connect(this.gateGainNode);
      this.gateGainNode.connect(this.destinationNode);
    } else {
      // Bypass chain straight to analyser & destination
      this.sourceNode.connect(this.analyserNode);
      this.analyserNode.connect(this.destinationNode);
    }
  }

  public setNoiseGateThreshold(db: number): void {
    if (typeof db !== 'number' || Number.isNaN(db)) return;
    const clampedDb = Math.max(-70, Math.min(-10, db));
    // Remember it so a pipeline created later (e.g. on load, before media is
    // ready) starts with the user's saved threshold instead of the default.
    // The metering loop reads this every frame, so live changes apply
    // immediately without touching the graph.
    this.thresholdDb = clampedDb;
    console.log(`[AUDIO PROCESSOR] Noise gate threshold set to ${clampedDb} dB`);
  }

  public getNoiseGateThreshold(): number {
    return this.thresholdDb;
  }

  public setEnabled(enabled: boolean): void {
    this.isEnabled = enabled;
    this.wirePipeline();
    console.log(`[AUDIO PROCESSOR] Processing ${enabled ? 'enabled' : 'bypassed'}`);
  }

  public getAudioLevel(): number {
    return this.currentLevel;
  }

  private startMetering(): void {
    if (!this.analyserNode) return;

    const dataArray = new Uint8Array(this.analyserNode.frequencyBinCount);

    const updateMeter = () => {
      if (!this.analyserNode) return;

      this.analyserNode.getByteFrequencyData(dataArray);

      let sum = 0;
      for (let i = 0; i < dataArray.length; i++) {
        sum += dataArray[i];
      }

      const average = sum / dataArray.length;
      // Normalize 0-255 to 0-100
      const level = Math.min(100, Math.round((average / 128) * 100));

      // P5: only push to the UI when the level actually changed — steady
      // silence or a constant tone no longer costs 60 style recalculations/s.
      if (shouldEmitLevel(this.currentLevel, level)) {
        this.currentLevel = level;
        this.eventBus.emit(Events.AUDIO_LEVEL, {
          target: 'local',
          level,
        });
      }

      // Drive the gate: silence below threshold, open above, with smooth
      // attack/release so words don't click or get chopped. setTargetAtTime
      // is only called when the open/close target flips.
      const target = computeGateTarget(level, this.thresholdDb, this.isEnabled);
      if (this.gateGainNode && this.audioCtx && target !== this.lastGateTarget) {
        this.gateGainNode.gain.setTargetAtTime(
          target,
          this.audioCtx.currentTime,
          target === 0 ? 0.05 : 0.02
        );
        this.lastGateTarget = target;
      }

      this.animFrameId = requestAnimationFrame(updateMeter);
    };

    updateMeter();
  }

  public destroy(): void {
    if (this.animFrameId !== null) {
      cancelAnimationFrame(this.animFrameId);
      this.animFrameId = null;
    }

    if (this.audioCtx && this.audioCtx.state !== 'closed') {
      this.audioCtx.close().catch(() => {});
    }

    this.audioCtx = null;
    this.sourceNode = null;
    this.hpfNode = null;
    this.gateGainNode = null;
    this.analyserNode = null;
    this.destinationNode = null;
    this.currentLevel = 0;
    this.lastGateTarget = null;
  }
}
