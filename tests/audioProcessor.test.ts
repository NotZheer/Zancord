// @vitest-environment happy-dom
import { describe, it, expect, beforeEach } from 'vitest';
import { AudioProcessor, thresholdToLevel, computeGateTarget, shouldEmitLevel } from '../src/core/AudioProcessor';
import { EventBus } from '../src/core/EventBus';
import { FakeMediaStream } from './helpers/fakeRtc';

class FakeParam {
  value: number;
  constructor(v: number) {
    this.value = v;
  }
  setValueAtTime(v: number) {
    this.value = v;
  }
  setTargetAtTime(v: number) {
    this.value = v;
  }
}

class FakeNode {
  type = 'highpass';
  frequency = new FakeParam(1000);
  Q = new FakeParam(0.7);
  gain = new FakeParam(1);
  fftSize = 128;
  smoothingTimeConstant = 0.8;
  frequencyBinCount = 128;
  stream = {};
  connect() {}
  disconnect() {}
  getByteFrequencyData(arr: Uint8Array) {
    arr.fill(50);
  }
}

class FakeAudioContext {
  static instances: FakeAudioContext[] = [];
  state = 'running';
  currentTime = 0;
  gains: FakeNode[] = [];
  constructor() {
    FakeAudioContext.instances.push(this);
  }
  createMediaStreamSource() {
    return new FakeNode();
  }
  createBiquadFilter() {
    return new FakeNode();
  }
  createGain() {
    const node = new FakeNode();
    this.gains.push(node);
    return node;
  }
  createAnalyser() {
    return new FakeNode();
  }
  createMediaStreamDestination() {
    return new FakeNode();
  }
  close() {
    this.state = 'closed';
    return Promise.resolve();
  }
}

function fakeStream() {
  return {
    getAudioTracks: () => [{ kind: 'audio' }],
    getVideoTracks: () => [],
  } as unknown as MediaStream;
}

beforeEach(() => {
  (window as any).AudioContext = FakeAudioContext;
  (window as any).MediaStream = FakeMediaStream;
  FakeAudioContext.instances = [];
});

describe('thresholdToLevel / computeGateTarget (real gate)', () => {
  it('maps -10 dB to meter level ~63 and -45 dB to ~1 (effectively off)', () => {
    expect(thresholdToLevel(-10)).toBeCloseTo(63, 0);
    expect(thresholdToLevel(-45)).toBeCloseTo(1.1, 1);
  });

  it('closes the gate below the threshold and opens above it', () => {
    expect(computeGateTarget(80, -10, true)).toBe(1); // loud → open
    expect(computeGateTarget(30, -10, true)).toBe(0); // quiet → closed
    expect(computeGateTarget(30, -45, true)).toBe(1); // very low threshold → open
  });

  it('stays open when processing is disabled', () => {
    expect(computeGateTarget(5, -10, false)).toBe(1);
  });
});

describe('shouldEmitLevel (P5: metering emission throttle)', () => {
  it('does not emit when the rounded level is unchanged', () => {
    expect(shouldEmitLevel(50, 50)).toBe(false);
    expect(shouldEmitLevel(39, 39.4)).toBe(false);
  });

  it('emits when the level moves by at least the delta', () => {
    expect(shouldEmitLevel(50, 51)).toBe(true);
    expect(shouldEmitLevel(50, 48)).toBe(true);
    expect(shouldEmitLevel(0, 1)).toBe(true);
  });

  it('supports a custom delta', () => {
    expect(shouldEmitLevel(50, 52, 3)).toBe(false);
    expect(shouldEmitLevel(50, 53, 3)).toBe(true);
  });
});

describe('AudioProcessor noise gate (threshold persistence)', () => {
  it('remembers a threshold set BEFORE the stream is processed (restore bug)', () => {
    const ap = new AudioProcessor(new EventBus());
    ap.setNoiseGateThreshold(-10);
    ap.processStream(fakeStream());
    expect(ap.getNoiseGateThreshold()).toBe(-10);
    ap.destroy();
  });

  it('defaults to -45 when no threshold was set', () => {
    const ap = new AudioProcessor(new EventBus());
    ap.processStream(fakeStream());
    expect(ap.getNoiseGateThreshold()).toBe(-45);
    ap.destroy();
  });

  it('clamps out-of-range values and ignores NaN', () => {
    const ap = new AudioProcessor(new EventBus());
    ap.setNoiseGateThreshold(-200);
    expect(ap.getNoiseGateThreshold()).toBe(-70);
    ap.setNoiseGateThreshold(NaN);
    expect(ap.getNoiseGateThreshold()).toBe(-70);
    ap.destroy();
  });

  it('builds the pipeline with the gate gain open at 1', () => {
    const ap = new AudioProcessor(new EventBus());
    ap.processStream(fakeStream());
    const ctx = FakeAudioContext.instances[0];
    expect(ctx.gains[0].gain.value).toBe(1);
    ap.destroy();
  });
});
