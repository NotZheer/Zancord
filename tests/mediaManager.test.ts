// @vitest-environment happy-dom
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { MediaManager } from '../src/core/MediaManager';
import { EventBus } from '../src/core/EventBus';
import { Events, ToastOptions } from '../src/types';

describe('MediaManager screen share (UX feedback)', () => {
  beforeEach(() => {
    (navigator as any).mediaDevices = {
      getDisplayMedia: vi.fn(),
    };
  });

  it('shows a friendly toast when the user cancels the screen picker', async () => {
    (navigator as any).mediaDevices.getDisplayMedia.mockRejectedValue(
      new DOMException('Permission denied by user', 'NotAllowedError')
    );
    const bus = new EventBus();
    const toasts: ToastOptions[] = [];
    bus.on(Events.TOAST, (t: ToastOptions) => toasts.push(t));
    const mm = new MediaManager(bus);

    const stream = await mm.startScreenShare();

    expect(stream).toBeNull();
    expect(toasts.length).toBe(1);
    expect(toasts[0].message).toContain('canceled');
    expect(toasts[0].type).toBe('info');
  });

  it('shows an error toast for unexpected screen share failures', async () => {
    (navigator as any).mediaDevices.getDisplayMedia.mockRejectedValue(
      new Error('Something exploded')
    );
    const bus = new EventBus();
    const toasts: ToastOptions[] = [];
    bus.on(Events.TOAST, (t: ToastOptions) => toasts.push(t));
    const mm = new MediaManager(bus);

    const stream = await mm.startScreenShare();

    expect(stream).toBeNull();
    expect(toasts[0].message).toContain('Could not start screen share');
    expect(toasts[0].type).toBe('error');
  });

  function mockDisplayStream(opts: { audio: boolean }) {
    const audioTrack = opts.audio ? [{ addEventListener: vi.fn() }] : [];
    return {
      getVideoTracks: () => [{ addEventListener: vi.fn() }],
      getAudioTracks: () => audioTrack,
      getTracks: () => [...audioTrack, { addEventListener: vi.fn() }],
    };
  }

  it('requests audio as a plain boolean when the share-audio toggle is on', async () => {
    (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(mockDisplayStream({ audio: true }));
    const mm = new MediaManager(new EventBus());

    await mm.startScreenShare(true);

    const constraints = (navigator as any).mediaDevices.getDisplayMedia.mock.calls[0][0];
    // Plain boolean, never a constraints object — object form is documented
    // to make some browsers silently drop the display-audio track.
    expect(constraints.audio).toBe(true);
    // Chrome-only: pre-request system audio for screen/window shares.
    expect(constraints.systemAudio).toBe('include');
  });

  it('does not request audio when the share-audio toggle is off', async () => {
    (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(mockDisplayStream({ audio: false }));
    const mm = new MediaManager(new EventBus());

    await mm.startScreenShare(false);

    const constraints = (navigator as any).mediaDevices.getDisplayMedia.mock.calls[0][0];
    expect(constraints.audio).toBe(false);
    expect(constraints.systemAudio).toBe('exclude');
  });

  it('warns when audio was requested but the browser did not provide it (after a grace period)', async () => {
    vi.useFakeTimers();
    try {
      (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(mockDisplayStream({ audio: false }));
      const bus = new EventBus();
      const toasts: ToastOptions[] = [];
      bus.on(Events.TOAST, (t: ToastOptions) => toasts.push(t));
      const mm = new MediaManager(bus);

      await mm.startScreenShare(true);
      // Browsers may deliver the track late — no instant warning.
      expect(toasts.length).toBe(0);

      vi.advanceTimersByTime(2000);
      expect(toasts.some((t) => t.message.toLowerCase().includes('audio'))).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not warn when the audio track arrives late', async () => {
    vi.useFakeTimers();
    try {
      const listeners: Record<string, ((e: unknown) => void)[]> = {};
      const tracks: { kind: string }[] = [];
      const stream = {
        getVideoTracks: () => [{ addEventListener: vi.fn() }],
        getAudioTracks: () => tracks.filter((t) => t.kind === 'audio'),
        getTracks: () => tracks,
        addEventListener: (name: string, cb: (e: unknown) => void) => {
          (listeners[name] ||= []).push(cb);
        },
      };
      (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(stream);
      const bus = new EventBus();
      const toasts: ToastOptions[] = [];
      bus.on(Events.TOAST, (t: ToastOptions) => toasts.push(t));
      const mm = new MediaManager(bus);

      await mm.startScreenShare(true);
      // Simulate the browser adding the audio track shortly after the picker.
      tracks.push({ kind: 'audio' });
      listeners['addtrack']?.forEach((cb) => cb({ track: { kind: 'audio' } }));
      vi.advanceTimersByTime(2000);
      expect(toasts.length).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('explains Safari cannot capture screen audio', async () => {
    vi.useFakeTimers();
    const originalUA = navigator.userAgent;
    try {
      Object.defineProperty(navigator, 'userAgent', {
        value:
          'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Safari/605.1.15',
        configurable: true,
      });
      (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(mockDisplayStream({ audio: false }));
      const bus = new EventBus();
      const toasts: ToastOptions[] = [];
      bus.on(Events.TOAST, (t: ToastOptions) => toasts.push(t));
      const mm = new MediaManager(bus);

      await mm.startScreenShare(true);
      vi.advanceTimersByTime(2000);

      expect(toasts.some((t) => t.message.includes('Safari'))).toBe(true);
    } finally {
      Object.defineProperty(navigator, 'userAgent', { value: originalUA, configurable: true });
      vi.useRealTimers();
    }
  });

  it('explains Linux Chrome can only carry audio from a tab share', async () => {
    vi.useFakeTimers();
    const originalUA = navigator.userAgent;
    try {
      Object.defineProperty(navigator, 'userAgent', {
        value:
          'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
        configurable: true,
      });
      (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(mockDisplayStream({ audio: false }));
      const bus = new EventBus();
      const toasts: ToastOptions[] = [];
      bus.on(Events.TOAST, (t: ToastOptions) => toasts.push(t));
      const mm = new MediaManager(bus);

      await mm.startScreenShare(true);
      vi.advanceTimersByTime(2000);

      const linuxToast = toasts.find((t) => t.message.toLowerCase().includes('linux'));
      expect(linuxToast).toBeDefined();
      expect(linuxToast?.message).toContain('tab');
    } finally {
      Object.defineProperty(navigator, 'userAgent', { value: originalUA, configurable: true });
      vi.useRealTimers();
    }
  });

  it('explains Firefox can only carry audio from a tab share', async () => {
    vi.useFakeTimers();
    const originalUA = navigator.userAgent;
    try {
      Object.defineProperty(navigator, 'userAgent', {
        value:
          'Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0',
        configurable: true,
      });
      (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(mockDisplayStream({ audio: false }));
      const bus = new EventBus();
      const toasts: ToastOptions[] = [];
      bus.on(Events.TOAST, (t: ToastOptions) => toasts.push(t));
      const mm = new MediaManager(bus);

      await mm.startScreenShare(true);
      vi.advanceTimersByTime(2000);

      const firefoxToast = toasts.find((t) => t.message.includes('Firefox'));
      expect(firefoxToast).toBeDefined();
      expect(firefoxToast?.message).toContain('tab');
    } finally {
      Object.defineProperty(navigator, 'userAgent', { value: originalUA, configurable: true });
      vi.useRealTimers();
    }
  });
});

describe('MediaManager screen share quality (PERF-AUDIT P1)', () => {
  function makeDisplayStream(opts: { track?: boolean }) {
    const applyConstraints = vi.fn().mockResolvedValue(undefined);
    const videoTrack =
      opts.track === false ? null : { kind: 'video', applyConstraints, addEventListener: vi.fn() };
    return {
      getVideoTracks: () => (videoTrack ? [videoTrack] : []),
      getAudioTracks: () => [],
      getTracks: () => (videoTrack ? [videoTrack] : []),
      applyConstraintsTrack: applyConstraints,
    };
  }

  it('applies the chosen resolution and fps as max-capped constraints', async () => {
    const stream = makeDisplayStream({ track: true });
    (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(stream);
    const mm = new MediaManager(new EventBus());
    await mm.startScreenShare(false);

    await mm.applyScreenShareConstraints({ width: 1280, height: 720, frameRate: 30 });

    expect(stream.applyConstraintsTrack).toHaveBeenCalledWith({
      width: { ideal: 1280, max: 1280 },
      height: { ideal: 720, max: 720 },
      frameRate: { ideal: 30, max: 30 },
    });
  });

  it('leaves width/height untouched for source-native', async () => {
    const stream = makeDisplayStream({ track: true });
    (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(stream);
    const mm = new MediaManager(new EventBus());
    await mm.startScreenShare(false);

    await mm.applyScreenShareConstraints({ width: null, height: null, frameRate: 15 });

    expect(stream.applyConstraintsTrack).toHaveBeenCalledWith({ frameRate: { ideal: 15, max: 15 } });
  });

  it('is a no-op when nothing is being shared (no video track)', async () => {
    const mm = new MediaManager(new EventBus());
    await expect(mm.applyScreenShareConstraints({ width: 1920, height: 1080, frameRate: 30 })).resolves.toBeUndefined();
  });

  it('surfaces applyConstraints failures as a warning toast', async () => {
    const stream = makeDisplayStream({ track: true });
    stream.applyConstraintsTrack.mockRejectedValue(new DOMException('Constraint not satisfiable', 'OverconstrainedError'));
    (navigator as any).mediaDevices.getDisplayMedia.mockResolvedValue(stream);
    const bus = new EventBus();
    const toasts: ToastOptions[] = [];
    bus.on(Events.TOAST, (t: ToastOptions) => toasts.push(t));
    const mm = new MediaManager(bus);
    await mm.startScreenShare(false);

    await mm.applyScreenShareConstraints({ width: 1920, height: 1080, frameRate: 60 });

    expect(toasts.some((t) => t.type === 'warning' && t.message.toLowerCase().includes('resolution'))).toBe(true);
  });
});
