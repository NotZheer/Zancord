// @vitest-environment happy-dom
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { RemoteAudioMeter } from '../src/core/RemoteAudioMeter';
import { EventBus } from '../src/core/EventBus';
import { Events, AudioLevelData } from '../src/types';

let signalLevel = 80;

class FakeParam {
  value = 1;
  setValueAtTime() {}
  setTargetAtTime() {}
}

class FakeNode {
  fftSize = 128;
  smoothingTimeConstant = 0.8;
  frequencyBinCount = 64;
  gain = new FakeParam();
  connect() {}
  disconnect() {}
  getByteFrequencyData(arr: Uint8Array) {
    arr.fill(signalLevel);
  }
}

class FakeAudioContext {
  static instances: FakeAudioContext[] = [];
  state = 'running';
  currentTime = 0;
  constructor() {
    FakeAudioContext.instances.push(this);
  }
  createMediaStreamSource() {
    return new FakeNode();
  }
  createAnalyser() {
    return new FakeNode();
  }
  createGain() {
    return new FakeNode();
  }
  get destination() {
    return new FakeNode();
  }
  resume() {
    this.state = 'running';
    return Promise.resolve();
  }
}

let rafCallback: FrameRequestCallback | null = null;

beforeEach(() => {
  (window as any).AudioContext = FakeAudioContext;
  FakeAudioContext.instances = [];
  rafCallback = null;
  signalLevel = 80;
  vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
    rafCallback = cb;
    return 1;
  });
  vi.stubGlobal('cancelAnimationFrame', () => {});
});

function fakeStream(withAudio: boolean) {
  return {
    getAudioTracks: () => (withAudio ? [{ kind: 'audio' }] : []),
  } as unknown as MediaStream;
}

describe('RemoteAudioMeter (U3: remote speaking indicator)', () => {
  it('emits AUDIO_LEVEL for a remote stream that carries signal', () => {
    const bus = new EventBus();
    const levels: AudioLevelData[] = [];
    bus.on(Events.AUDIO_LEVEL, (d: AudioLevelData) => levels.push(d));
    const meter = new RemoteAudioMeter(bus);
    meter.attach('p1-screen', fakeStream(true));

    rafCallback?.(0); // first frame — establishes baseline
    rafCallback?.(0); // unchanged data — throttled
    rafCallback?.(0);

    expect(levels.length).toBeGreaterThan(0);
    expect(levels[0].target).toBe('p1-screen');
    // 80/128*100 ≈ 63
    expect(levels[0].level).toBe(63);
  });

  it('does not emit while the level is unchanged (throttled)', () => {
    const bus = new EventBus();
    const levels: AudioLevelData[] = [];
    bus.on(Events.AUDIO_LEVEL, (d: AudioLevelData) => levels.push(d));
    const meter = new RemoteAudioMeter(bus);
    meter.attach('p1', fakeStream(true));

    rafCallback?.(0);
    rafCallback?.(0);
    rafCallback?.(0);

    expect(levels.length).toBe(1);
  });

  it('emits again when the level changes', () => {
    const bus = new EventBus();
    const levels: AudioLevelData[] = [];
    bus.on(Events.AUDIO_LEVEL, (d: AudioLevelData) => levels.push(d));
    const meter = new RemoteAudioMeter(bus);
    meter.attach('p1', fakeStream(true));

    rafCallback?.(0);
    signalLevel = 30;
    rafCallback?.(0);

    expect(levels.length).toBe(2);
    expect(levels[1].level).toBe(23); // 30/128*100 ≈ 23
  });

  it('ignores streams without an audio track', () => {
    const bus = new EventBus();
    const levels: AudioLevelData[] = [];
    bus.on(Events.AUDIO_LEVEL, (d: AudioLevelData) => levels.push(d));
    const meter = new RemoteAudioMeter(bus);
    meter.attach('p1-screen', fakeStream(false));

    rafCallback?.(0);

    expect(levels.length).toBe(0);
  });

  it('detach stops metering', () => {
    const bus = new EventBus();
    const levels: AudioLevelData[] = [];
    bus.on(Events.AUDIO_LEVEL, (d: AudioLevelData) => levels.push(d));
    const meter = new RemoteAudioMeter(bus);
    meter.attach('p1', fakeStream(true));
    meter.detach('p1');

    rafCallback?.(0);

    expect(levels.length).toBe(0);
  });

  it('does not double-meter the same peer id', () => {
    const bus = new EventBus();
    const levels: AudioLevelData[] = [];
    bus.on(Events.AUDIO_LEVEL, (d: AudioLevelData) => levels.push(d));
    const meter = new RemoteAudioMeter(bus);
    meter.attach('p1-screen', fakeStream(true));
    meter.attach('p1-screen', fakeStream(true));

    rafCallback?.(0);
    rafCallback?.(0);

    expect(levels.length).toBe(1);
    expect(FakeAudioContext.instances.length).toBe(1);
  });

  it('detachAll clears every meter', () => {
    const bus = new EventBus();
    const levels: AudioLevelData[] = [];
    bus.on(Events.AUDIO_LEVEL, (d: AudioLevelData) => levels.push(d));
    const meter = new RemoteAudioMeter(bus);
    meter.attach('p1', fakeStream(true));
    meter.attach('p2-screen', fakeStream(true));
    meter.detachAll();

    rafCallback?.(0);

    expect(levels.length).toBe(0);
  });
});
