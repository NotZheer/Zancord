import { describe, it, expect } from 'vitest';
import { screenAudioSupport, supportsScreenAudio, screenAudioHint } from '../src/utils/browser';

describe('supportsScreenAudio (Safari UA detection)', () => {
  it('returns true for Chrome', () => {
    expect(
      supportsScreenAudio(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
      )
    ).toBe(true);
  });

  it('returns true for Edge (Chromium)', () => {
    expect(
      supportsScreenAudio(
        'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0'
      )
    ).toBe(true);
  });

  it('returns true for Firefox', () => {
    expect(
      supportsScreenAudio('Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:121.0) Gecko/20100101 Firefox/121.0')
    ).toBe(true);
  });

  it('returns false for Safari on macOS', () => {
    expect(
      supportsScreenAudio(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Safari/605.1.15'
      )
    ).toBe(false);
  });

  it('returns "safari" for Safari on iOS', () => {
    expect(
      supportsScreenAudio(
        'Mozilla/5.0 (iPhone; CPU iPhone OS 17_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Mobile/15E148 Safari/604.1'
      )
    ).toBe(false);
  });
});

describe('screenAudioHint (accurate per-browser + per-OS guidance)', () => {
  it('tells Firefox-on-Linux users that ONLY a tab share carries audio', () => {
    const hint = screenAudioHint(
      'Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0'
    );
    expect(hint).toContain('Firefox');
    expect(hint.toLowerCase()).toContain('tab');
    // Must NOT recommend Chrome/Edge for screen+audio on Linux.
    expect(hint.toLowerCase()).not.toContain('chrome/edge for screen');
  });

  it('tells Firefox-on-macOS users a tab share OR Chrome/Edge screen works', () => {
    const hint = screenAudioHint(
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:121.0) Gecko/20100101 Firefox/121.0'
    );
    expect(hint).toContain('Firefox');
    expect(hint.toLowerCase()).toContain('chrome/edge');
  });

  it('tells Linux-Chromium users to share a tab (no screen+audio on Linux)', () => {
    const hint = screenAudioHint(
      'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
    );
    expect(hint.toLowerCase()).toContain('tab');
    expect(hint.toLowerCase()).toContain('linux');
    // Must mention the Chrome launch flag that enables screen+system audio.
    expect(hint).toContain('--enable-features');
  });

  it('tells Safari users it cannot capture screen audio at all', () => {
    expect(
      screenAudioHint(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Safari/605.1.15'
      )
    ).toContain('Safari');
  });

  it('gives generic picker guidance to fully-supported browsers', () => {
    expect(
      screenAudioHint(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
      ).toLowerCase()
    ).toContain('picker');
  });
});

describe('screenAudioSupport (platform-aware detection)', () => {
  it('returns "supported" for Chrome on macOS', () => {
    expect(
      screenAudioSupport(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
      )
    ).toBe('supported');
  });

  it('returns "supported" for Edge on Windows', () => {
    expect(
      screenAudioSupport(
        'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0'
      )
    ).toBe('supported');
  });

  it('returns "linux" for Chrome on Linux (tab shares are the only audio path)', () => {
    expect(
      screenAudioSupport(
        'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
      )
    ).toBe('linux');
  });

  it('returns "linux" for Chromium on Linux', () => {
    expect(
      screenAudioSupport(
        'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chromium/120.0.0.0 Safari/537.36'
      )
    ).toBe('linux');
  });

  it('returns "linux" for Edge on Linux', () => {
    expect(
      screenAudioSupport(
        'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0'
      )
    ).toBe('linux');
  });

  it('returns "firefox" for Firefox on Linux (screen/window shares never carry audio)', () => {
    expect(
      screenAudioSupport('Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0')
    ).toBe('firefox');
  });

  it('returns "firefox" for Firefox on macOS too (same limitation, all OSes)', () => {
    expect(
      screenAudioSupport('Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:121.0) Gecko/20100101 Firefox/121.0')
    ).toBe('firefox');
  });

  it('returns "safari" for Safari on macOS', () => {
    expect(
      screenAudioSupport(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Safari/605.1.15'
      )
    ).toBe('safari');
  });
});
