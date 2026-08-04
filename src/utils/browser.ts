const SAFARI_UA = /^((?!chrome|chromium|android|crios|fxios|edg).)*safari/i;
const LINUX_UA = /linux/i;
const CHROMIUM_UA = /chrom(e|ium)|crios/i;
const FIREFOX_UA = /firefox|fxios/i;

/**
 * Why a browser can't carry screen-share audio — or which source type it
 * needs. There is no capability API for display-capture audio, so we sniff
 * the UA (same tradeoff as the Safari check below).
 *
 * Truth table for `getDisplayMedia({ audio: true })`:
 *   Chrome/Edge (macOS/Windows) — screen/window AND tab shares can carry
 *     audio (picker checkbox).
 *   Linux (ANY browser)          — only TAB shares carry audio; the picker
 *     offers no audio for screen/window sources, no matter which browser.
 *     This is a platform limitation, not a browser setting.
 *   Firefox (macOS/Windows)      — only TAB shares carry audio (picker
 *     checkbox). Screen/window shares NEVER carry audio.
 *   Safari (all)                 — no screen audio at all.
 */
export type ScreenAudioSupport = 'supported' | 'safari' | 'firefox' | 'linux';

export function screenAudioSupport(userAgent: string): ScreenAudioSupport {
  if (SAFARI_UA.test(userAgent)) return 'safari';
  // Firefox's display-capture API exposes audio ONLY for tab shares — a
  // Firefox implementation limitation on every OS.
  if (FIREFOX_UA.test(userAgent)) return 'firefox';
  if (LINUX_UA.test(userAgent) && CHROMIUM_UA.test(userAgent)) return 'linux';
  return 'supported';
}

/**
 * User-facing guidance for a browser that received no screen-audio track.
 * Takes the UA (not just the tier) because Firefox's options differ by OS:
 * on macOS/Windows Chrome/Edge CAN do screen + system audio; on Linux no
 * browser can.
 */
export function screenAudioHint(userAgent: string): string {
  const isLinux = LINUX_UA.test(userAgent);
  switch (screenAudioSupport(userAgent)) {
    case 'safari':
      return 'Safari cannot capture screen audio — open the invite link in Chrome, Edge or Firefox to share audio with your screen.';
    case 'firefox':
      return isLinux
        ? 'Firefox on Linux only carries audio from a TAB — pick a tab and tick "Share audio". No Linux browser adds audio to a screen/window share.'
        : 'Firefox only carries audio from a TAB — pick a tab and tick "Share audio", or use Chrome/Edge for screen + system audio.';
    case 'linux':
      // Chromium on Linux only supports audio from tab capture — the
      // picker has no "Share audio" checkbox for screen/window picks.
      // The one escape hatch: launch Chrome with the Pulseaudio loopback
      // feature flag, which adds an "Also share system audio" checkbox.
      return 'On Linux, only Chrome can add audio to a screen share — launch it with --enable-features=PulseaudioLoopbackForScreenShare and tick "Also share system audio" (Firefox: tab shares only). Or pick a "Monitor of …" source as your mic in Settings.';
    default:
      return 'No screen audio captured — enable "Share tab/system audio" in the picker.';
  }
}

/**
 * Whether a browser can carry screen-share audio AT ALL (from any source
 * type). Safari is the only major browser that cannot capture audio in
 * getDisplayMedia — its audio constraint is silently ignored and the picker
 * has no audio checkbox. There is no capability API for this, so UA sniffing
 * is the only option.
 *
 * Note: "Chromium" UA strings also end in "Safari/537.36" but are NOT Safari,
 * hence the explicit chromium exclusion.
 */
export function supportsScreenAudio(userAgent: string): boolean {
  return !SAFARI_UA.test(userAgent);
}
