import { ScreenShareQuality } from '../types';
import { screenAudioHint } from '../utils/browser';

/**
 * window.localStorage may be unavailable (privacy modes, some webviews) or
 * throw on write — fall back to a per-instance in-memory store so the share
 * flow never breaks because persistence failed.
 */
function safeStorage(): Storage | null {
  try {
    const probe = '__zancord_storage_probe__';
    window.localStorage.setItem(probe, '1');
    window.localStorage.removeItem(probe);
    return window.localStorage;
  } catch {
    return null;
  }
}

/**
 * Modal shown AFTER the browser's screen/window/tab picker closes, letting
 * the user cap the share's resolution and frame rate before it goes out to
 * peers (PERF-AUDIT P1 — the picker delivers the display's native
 * resolution, which is often 4K and far more expensive to encode than
 * needed). `open()` resolves with the chosen quality, or `null` if the
 * user cancels — the caller then stops the captured stream.
 */
export class ShareQualityModal {
  private overlay: HTMLElement;
  private video: HTMLVideoElement;
  private resolutionSelect: HTMLSelectElement;
  private fpsSelect: HTMLSelectElement;
  private storage: Storage | null;
  private resolveOpen: ((quality: ScreenShareQuality | null) => void) | null = null;
  private activeStream: MediaStream | null = null;
  private onTrackEnded: (() => void) | null = null;
  private onStreamAddTrack: (() => void) | null = null;

  private static readonly RESOLUTIONS: Array<{ value: string; label: string; width: number | null; height: number | null }> = [
    { value: 'source', label: 'Source native', width: null, height: null },
    { value: '1920', label: '1920 × 1080 (1080p)', width: 1920, height: 1080 },
    { value: '1280', label: '1280 × 720 (720p)', width: 1280, height: 720 },
    { value: '960', label: '960 × 540', width: 960, height: 540 },
    { value: '640', label: '640 × 360', width: 640, height: 360 },
  ];

  private static readonly FPS_OPTIONS = [60, 30, 15, 10];

  constructor(storage: Storage | null = null) {
    this.storage = storage ?? safeStorage();
    this.overlay = document.createElement('div');
    this.overlay.className = 'share-quality-overlay';
    this.overlay.hidden = true;
    this.overlay.innerHTML = `
      <section class="share-quality-modal" role="dialog" aria-modal="true" aria-label="Screen share settings">
        <header class="share-quality-header">
          <h3><i class="fa-solid fa-display" aria-hidden="true"></i> SCREEN SHARE SETTINGS</h3>
          <p class="share-quality-sub">Choose the quality peers will receive. Lower = smoother on slow links.</p>
        </header>
        <video class="share-quality-preview" autoplay muted playsinline></video>
        <p class="share-quality-audio-hint" hidden>
          <i class="fa-solid fa-triangle-exclamation" aria-hidden="true"></i>
          <span></span>
        </p>
        <div class="share-quality-fields">
          <label class="share-quality-field">
            <span>RESOLUTION</span>
            <select id="share-quality-resolution"></select>
          </label>
          <label class="share-quality-field">
            <span>FRAME RATE</span>
            <select id="share-quality-fps"></select>
          </label>
        </div>
        <footer class="share-quality-actions">
          <button type="button" class="share-quality-cancel">Cancel</button>
          <button type="button" class="share-quality-confirm">Start Sharing</button>
        </footer>
      </section>
    `;

    this.video = this.overlay.querySelector('.share-quality-preview') as HTMLVideoElement;
    this.resolutionSelect = this.overlay.querySelector('#share-quality-resolution') as HTMLSelectElement;
    this.fpsSelect = this.overlay.querySelector('#share-quality-fps') as HTMLSelectElement;

    // Resolution options; the "source native" label is enriched with the
    // actual captured size once a stream is attached.
    ShareQualityModal.RESOLUTIONS.forEach((r) => {
      const option = document.createElement('option');
      option.value = r.value;
      option.textContent = r.label;
      this.resolutionSelect.appendChild(option);
    });
    ShareQualityModal.FPS_OPTIONS.forEach((fps) => {
      const option = document.createElement('option');
      option.value = String(fps);
      option.textContent = `${fps} FPS`;
      this.fpsSelect.appendChild(option);
    });

    this.overlay.querySelector('.share-quality-cancel')?.addEventListener('click', () => this.cancel());
    this.overlay.querySelector('.share-quality-confirm')?.addEventListener('click', () => this.confirm());
    this.overlay.addEventListener('click', (event) => {
      if (event.target === this.overlay) this.cancel();
    });
    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && !this.overlay.hidden) {
        event.preventDefault();
        this.cancel();
      }
    });

    document.body.appendChild(this.overlay);
  }

  /**
   * Shows the modal with a live preview. Resolves with the chosen quality,
   * or `null` on cancel / Escape / backdrop click / track-ended.
   * `expectAudio` = the share-audio toggle is on; if the browser returned
   * no audio track, an inline hint explains which source type to pick
   * instead (e.g. Firefox only carries audio from tabs).
   */
  public open(stream: MediaStream, expectAudio = false): Promise<ScreenShareQuality | null> {
    this.activeStream = stream;
    const track = stream.getVideoTracks()[0];

    // Live preview (muted — the real audio goes out on confirm).
    this.video.srcObject = stream;
    this.video.muted = true;
    this.video.play().catch(() => {});

    this.populateResolutionLabel(track);
    this.showAudioHint(stream, expectAudio);

    // Chrome on Linux can deliver the loopback audio track a beat AFTER the
    // picker closes — if it lands while the modal is open, clear the warning.
    if (
      expectAudio &&
      stream.getAudioTracks().length === 0 &&
      typeof stream.addEventListener === 'function'
    ) {
      this.onStreamAddTrack = () => {
        if (stream.getAudioTracks().length > 0) this.hideAudioHint();
      };
      stream.addEventListener('addtrack', this.onStreamAddTrack);
    }

    // Restore last choice (perf-friendly default: 1080p30).
    const savedResolution = this.storage?.getItem('zancord_share_resolution') || '1920';
    const savedFps = this.storage?.getItem('zancord_share_fps') || '30';
    if (Array.from(this.resolutionSelect.options).some((o) => o.value === savedResolution)) {
      this.resolutionSelect.value = savedResolution;
    }
    if (Array.from(this.fpsSelect.options).some((o) => o.value === savedFps)) {
      this.fpsSelect.value = savedFps;
    }

    this.overlay.hidden = false;
    (this.overlay.querySelector('.share-quality-confirm') as HTMLButtonElement | null)?.focus();

    // If the user stops sharing from the browser UI while the modal is open,
    // treat it as a cancel so we don't leave a dead preview up.
    if (track && typeof track.addEventListener === 'function') {
      this.onTrackEnded = () => this.cancel();
      track.addEventListener('ended', this.onTrackEnded);
    }

    return new Promise<ScreenShareQuality | null>((resolve) => {
      this.resolveOpen = resolve;
    });
  }

  private showAudioHint(stream: MediaStream, expectAudio: boolean): void {
    const hint = this.overlay.querySelector('.share-quality-audio-hint') as HTMLElement;
    const text = hint.querySelector('span') as HTMLElement;
    if (!expectAudio || stream.getAudioTracks().length > 0) {
      hint.hidden = true;
      return;
    }
    text.textContent = screenAudioHint(navigator.userAgent);
    hint.hidden = false;
  }

  private hideAudioHint(): void {
    const hint = this.overlay.querySelector('.share-quality-audio-hint') as HTMLElement;
    hint.hidden = true;
  }

  private populateResolutionLabel(track: MediaStreamTrack | undefined): void {
    const sourceOption = this.resolutionSelect.querySelector('option[value="source"]') as HTMLOptionElement | null;
    if (!sourceOption) return;
    const settings = typeof track?.getSettings === 'function' ? track.getSettings() : null;
    if (settings && settings.width && settings.height) {
      sourceOption.textContent = `Source native (${settings.width} × ${settings.height})`;
    }
  }

  private readQuality(): ScreenShareQuality {
    const resolution = ShareQualityModal.RESOLUTIONS.find((r) => r.value === this.resolutionSelect.value);
    const fps = Number(this.fpsSelect.value) || null;
    return {
      width: resolution?.width ?? null,
      height: resolution?.height ?? null,
      frameRate: fps,
    };
  }

  private confirm(): void {
    const quality = this.readQuality();
    this.storage?.setItem('zancord_share_resolution', this.resolutionSelect.value);
    this.storage?.setItem('zancord_share_fps', this.fpsSelect.value);
    this.close(quality);
  }

  private cancel(): void {
    this.close(null);
  }

  private close(result: ScreenShareQuality | null): void {
    if (this.onTrackEnded && this.activeStream) {
      const track = this.activeStream.getVideoTracks()[0];
      track?.removeEventListener?.('ended', this.onTrackEnded);
    }
    if (this.onStreamAddTrack && this.activeStream) {
      this.activeStream.removeEventListener?.('addtrack', this.onStreamAddTrack);
    }
    this.onTrackEnded = null;
    this.onStreamAddTrack = null;
    this.activeStream = null;
    this.video.srcObject = null;
    this.overlay.hidden = true;
    this.resolveOpen?.(result);
    this.resolveOpen = null;
  }
}
