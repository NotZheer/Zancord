import { EventBus } from './EventBus';
import { Events, AudioLevelData } from '../types';
import { shouldEmitLevel } from './AudioProcessor';

interface MeterHandle {
  source: MediaStreamAudioSourceNode;
  analyser: AnalyserNode;
  gain: GainNode;
  data: Uint8Array<ArrayBuffer>;
  lastLevel: number;
  raf: number;
  peerId: string;
}

/**
 * Per-remote-stream audio metering (AUDIT U3): attaches a silent Web Audio
 * tap to each remote stream's audio track and emits `AUDIO_LEVEL` so the
 * speaking ring lights up on the RIGHT tile — including screen-share cards.
 *
 * This is how we can SEE whether a remote screen share actually carries
 * sound: if the ring pulses on the screen card, the track has signal; if
 * it never pulses, the capture itself is silent.
 *
 * The gain tap is silent (gain 0) — the <video> element plays the real
 * audio; Web Audio here only measures.
 */
export class RemoteAudioMeter {
  private ctx: AudioContext | null = null;
  private meters: Map<string, MeterHandle> = new Map();
  private eventBus: EventBus;

  constructor(eventBus: EventBus) {
    this.eventBus = eventBus;
  }

  private ensureContext(): AudioContext | null {
    if (!this.ctx) {
      const Ctor = window.AudioContext || (window as any).webkitAudioContext;
      if (!Ctor) return null;
      this.ctx = new Ctor();
    }
    if (this.ctx.state === 'suspended') {
      this.ctx.resume().catch(() => {});
    }
    return this.ctx;
  }

  public attach(peerId: string, stream: MediaStream): void {
    const track = stream.getAudioTracks()[0];
    if (!track || this.meters.has(peerId)) return;

    const ctx = this.ensureContext();
    if (!ctx) return;

    try {
      const source = ctx.createMediaStreamSource(new MediaStream([track]));
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 256;
      analyser.smoothingTimeConstant = 0.8;
      const gain = ctx.createGain();
      gain.gain.value = 0; // silent tap — the element plays the real audio

      source.connect(analyser);
      analyser.connect(gain);
      gain.connect(ctx.destination);

      const handle: MeterHandle = {
        peerId,
        source,
        analyser,
        gain,
        data: new Uint8Array(analyser.frequencyBinCount),
        lastLevel: -1,
        raf: 0,
      };
      this.meters.set(peerId, handle);

      const loop = () => {
        if (!this.meters.has(peerId)) return;
        analyser.getByteFrequencyData(handle.data);
        let sum = 0;
        for (let i = 0; i < handle.data.length; i++) sum += handle.data[i];
        const level = Math.min(100, Math.round((sum / handle.data.length / 128) * 100));
        if (shouldEmitLevel(handle.lastLevel, level)) {
          handle.lastLevel = level;
          this.eventBus.emit(Events.AUDIO_LEVEL, {
            target: peerId,
            level,
          } as AudioLevelData);
        }
        handle.raf = requestAnimationFrame(loop);
      };
      handle.raf = requestAnimationFrame(loop);
    } catch (err) {
      console.warn('[METER] Failed to attach analyser:', err);
    }
  }

  public detach(peerId: string): void {
    const handle = this.meters.get(peerId);
    if (!handle) return;
    cancelAnimationFrame(handle.raf);
    try {
      handle.source.disconnect();
      handle.analyser.disconnect();
      handle.gain.disconnect();
    } catch {
      // nodes may already be disconnected
    }
    this.meters.delete(peerId);
  }

  public detachAll(): void {
    [...this.meters.keys()].forEach((id) => this.detach(id));
  }
}
