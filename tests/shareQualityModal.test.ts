// @vitest-environment happy-dom
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { ShareQualityModal } from '../src/ui/ShareQualityModal';

// happy-dom validates srcObject against its internal MediaStream class;
// stub it like the uiRenderer suite does.
Object.defineProperty(window.HTMLMediaElement.prototype, 'srcObject', {
  configurable: true,
  writable: true,
  value: null,
});

/** In-memory Storage stub — happy-dom's vitest env has no localStorage. */
function makeStorage(initial: Record<string, string> = {}): Storage {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
    clear: () => map.clear(),
    key: (i: number) => [...map.keys()][i] ?? null,
    get length() {
      return map.size;
    },
  } as Storage;
}

function fakeScreenStream(opts: { settings?: Record<string, unknown>; audio?: boolean } = {}) {
  const endedListeners: Array<() => void> = [];
  const streamListeners: Record<string, Array<() => void>> = {};
  const audioTracks: Array<{ kind: string }> = opts.audio ? [{ kind: 'audio' }] : [];
  const videoTrack = {
    kind: 'video',
    getSettings: () => opts.settings ?? { width: 3840, height: 2160, frameRate: 60 },
    addEventListener: (name: string, cb: () => void) => {
      if (name === 'ended') endedListeners.push(cb);
    },
  };
  return {
    stream: {
      getVideoTracks: () => [videoTrack],
      getAudioTracks: () => audioTracks,
      getTracks: () => [videoTrack, ...audioTracks],
      addEventListener: (name: string, cb: () => void) => {
        (streamListeners[name] ||= []).push(cb);
      },
      removeEventListener: (name: string, cb: () => void) => {
        streamListeners[name] = (streamListeners[name] || []).filter((f) => f !== cb);
      },
    },
    fireEnded: () => endedListeners.forEach((cb) => cb()),
    fireAddTrack: () => (streamListeners['addtrack'] || []).forEach((cb) => cb()),
    addAudioTrack: () => {
      audioTracks.push({ kind: 'audio' });
    },
  };
}

beforeEach(() => {
  document.body.innerHTML = '';
});

describe('ShareQualityModal (PERF-AUDIT P1: pick resolution/FPS after source selection)', () => {
  it('opens over the app with a muted live preview of the captured stream', () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream } = fakeScreenStream();
    modal.open(stream);

    const overlay = document.querySelector('.share-quality-overlay') as HTMLElement;
    expect(overlay.hidden).toBe(false);
    const video = overlay.querySelector('video') as HTMLVideoElement;
    expect(video.srcObject).toBe(stream);
    expect(video.muted).toBe(true);
  });

  it('defaults to 1080p30 when nothing was saved before', async () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream } = fakeScreenStream();
    const promise = modal.open(stream);

    const resolution = document.querySelector('#share-quality-resolution') as HTMLSelectElement;
    const fps = document.querySelector('#share-quality-fps') as HTMLSelectElement;
    expect(resolution.value).toBe('1920');
    expect(fps.value).toBe('30');

    (document.querySelector('.share-quality-confirm') as HTMLButtonElement).click();
    const quality = await promise;
    expect(quality).toEqual({ width: 1920, height: 1080, frameRate: 30 });
  });

  it('restores the saved resolution and fps', () => {
    const storage = makeStorage({ zancord_share_resolution: '1280', zancord_share_fps: '60' });
    const modal = new ShareQualityModal(storage);
    const { stream } = fakeScreenStream();
    modal.open(stream);

    const resolution = document.querySelector('#share-quality-resolution') as HTMLSelectElement;
    const fps = document.querySelector('#share-quality-fps') as HTMLSelectElement;
    expect(resolution.value).toBe('1280');
    expect(fps.value).toBe('60');
  });

  it('confirm resolves the chosen quality and persists it', async () => {
    const storage = makeStorage();
    const modal = new ShareQualityModal(storage);
    const { stream } = fakeScreenStream();
    const promise = modal.open(stream);

    const resolution = document.querySelector('#share-quality-resolution') as HTMLSelectElement;
    const fps = document.querySelector('#share-quality-fps') as HTMLSelectElement;
    resolution.value = '640';
    fps.value = '15';
    (document.querySelector('.share-quality-confirm') as HTMLButtonElement).click();

    const quality = await promise;
    expect(quality).toEqual({ width: 640, height: 360, frameRate: 15 });
    expect(storage.getItem('zancord_share_resolution')).toBe('640');
    expect(storage.getItem('zancord_share_fps')).toBe('15');
    // Modal closes and stops rendering the preview.
    expect((document.querySelector('.share-quality-overlay') as HTMLElement).hidden).toBe(true);
    const video = document.querySelector('.share-quality-modal video') as HTMLVideoElement;
    expect(video.srcObject).toBeNull();
  });

  it('source-native maps to null width/height (leave capture untouched)', async () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream } = fakeScreenStream();
    const promise = modal.open(stream);

    const resolution = document.querySelector('#share-quality-resolution') as HTMLSelectElement;
    resolution.value = 'source';
    (document.querySelector('.share-quality-confirm') as HTMLButtonElement).click();

    expect(await promise).toEqual({ width: null, height: null, frameRate: 30 });
  });

  it('cancel resolves null so the caller stops the stream', async () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream } = fakeScreenStream();
    const promise = modal.open(stream);

    (document.querySelector('.share-quality-cancel') as HTMLButtonElement).click();
    expect(await promise).toBeNull();
  });

  it('Escape cancels the share', async () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream } = fakeScreenStream();
    const promise = modal.open(stream);

    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(await promise).toBeNull();
  });

  it('clicking the backdrop cancels the share', async () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream } = fakeScreenStream();
    const promise = modal.open(stream);

    (document.querySelector('.share-quality-overlay') as HTMLElement).dispatchEvent(
      new MouseEvent('click', { bubbles: true })
    );
    expect(await promise).toBeNull();
  });

  it('auto-cancels when the captured track ends (browser "stop sharing" while modal open)', async () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream, fireEnded } = fakeScreenStream();
    const promise = modal.open(stream);

    fireEnded();
    expect(await promise).toBeNull();
  });

  it('warns inline when audio was expected but the capture has none (Firefox tab-only rule)', () => {
    const originalUA = navigator.userAgent;
    try {
      Object.defineProperty(navigator, 'userAgent', {
        value:
          'Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0',
        configurable: true,
      });
      const modal = new ShareQualityModal(makeStorage());
      const { stream } = fakeScreenStream({ audio: false });
      modal.open(stream, true);

      const hint = document.querySelector('.share-quality-audio-hint') as HTMLElement;
      expect(hint.hidden).toBe(false);
      expect(hint.textContent).toContain('Firefox');
      expect(hint.textContent).toContain('tab');
    } finally {
      Object.defineProperty(navigator, 'userAgent', { value: originalUA, configurable: true });
    }
  });

  it('shows no hint when the capture already carries audio', () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream } = fakeScreenStream({ audio: true });
    modal.open(stream, true);

    expect((document.querySelector('.share-quality-audio-hint') as HTMLElement).hidden).toBe(true);
  });

  it('shows no hint when screen audio was not requested', () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream } = fakeScreenStream({ audio: false });
    modal.open(stream, false);

    expect((document.querySelector('.share-quality-audio-hint') as HTMLElement).hidden).toBe(true);
  });

  it('clears the inline warning when the audio track arrives late (Chrome Linux loopback)', () => {
    const modal = new ShareQualityModal(makeStorage());
    const { stream, fireAddTrack, addAudioTrack } = fakeScreenStream({ audio: false });
    modal.open(stream, true);
    const hint = document.querySelector('.share-quality-audio-hint') as HTMLElement;
    expect(hint.hidden).toBe(false);

    // Chrome on Linux can deliver the loopback audio track a beat after the
    // picker — the warning should disappear once it lands.
    addAudioTrack();
    fireAddTrack();
    expect(hint.hidden).toBe(true);
  });
});
