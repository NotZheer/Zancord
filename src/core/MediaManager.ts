import { EventBus } from './EventBus';
import { Events, MediaDevices, ScreenShareQuality, ToastOptions } from '../types';
import { screenAudioHint } from '../utils/browser';

export class MediaManager {
  private localStream: MediaStream | null = null;
  private screenStream: MediaStream | null = null;
  private eventBus: EventBus;

  constructor(eventBus: EventBus) {
    this.eventBus = eventBus;
  }

  public getLocalStream(): MediaStream | null {
    return this.localStream;
  }

  public async initLocalMedia(wantVideo = true, wantAudio = true): Promise<MediaStream> {
    console.log('[MEDIA] Initializing local media (Audio separate from Video)...');
    this.localStream = new MediaStream();

    if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
      console.error('[MEDIA] navigator.mediaDevices is not available (Unsecure context or HTTP IP address).');
      this.eventBus.emit(Events.TOAST, {
        message: 'Camera/Mic blocked on HTTP IP addresses by browser security. Use http://localhost:5173 or enable chrome://flags/#unsafely-treat-insecure-origin-as-secure',
        type: 'error',
        duration: 8000,
      });
      return this.localStream;
    }

    let audioTrack: MediaStreamTrack | null = null;
    let videoTrack: MediaStreamTrack | null = null;

    // Acquire Audio
    if (wantAudio) {
      try {
        const audioStream = await navigator.mediaDevices.getUserMedia({
          audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
        });
        audioTrack = audioStream.getAudioTracks()[0] || null;
      } catch (err) {
        console.warn('[MEDIA] Ideal audio constraints failed, trying fallback audio: true...', err);
        try {
          const fallbackAudio = await navigator.mediaDevices.getUserMedia({ audio: true });
          audioTrack = fallbackAudio.getAudioTracks()[0] || null;
        } catch (fallbackErr) {
          console.error('[MEDIA] Could not acquire audio:', fallbackErr);
        }
      }
    }

    // Acquire Video
    if (wantVideo) {
      try {
        const videoStream = await navigator.mediaDevices.getUserMedia({
          video: { width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } },
        });
        videoTrack = videoStream.getVideoTracks()[0] || null;
      } catch (err) {
        console.warn('[MEDIA] Ideal video constraints failed, trying fallback video: true...', err);
        try {
          const fallbackVideo = await navigator.mediaDevices.getUserMedia({ video: true });
          videoTrack = fallbackVideo.getVideoTracks()[0] || null;
        } catch (fallbackErr) {
          console.error('[MEDIA] Could not acquire video:', fallbackErr);
        }
      }
    }

    if (audioTrack) this.localStream.addTrack(audioTrack);
    if (videoTrack) this.localStream.addTrack(videoTrack);

    return this.localStream;
  }

  public toggleMicrophone(enabled: boolean): void {
    if (!this.localStream) return;
    this.localStream.getAudioTracks().forEach((track) => {
      track.enabled = enabled;
    });
    console.log(`[MEDIA] Microphone ${enabled ? 'enabled' : 'disabled'}`);
  }

  public toggleCamera(enabled: boolean): void {
    if (!this.localStream) return;
    this.localStream.getVideoTracks().forEach((track) => {
      track.enabled = enabled;
    });
    console.log(`[MEDIA] Camera ${enabled ? 'enabled' : 'disabled'}`);
  }

  /**
   * Start screen sharing. When `includeAudio` is on, system/tab audio is
   * requested from the browser — but the browser may still omit it (e.g. the
   * user didn't check "Share audio" in the picker), in which case we warn.
   */
  public async startScreenShare(includeAudio = false): Promise<MediaStream | null> {
    try {
      console.log('[MEDIA] Starting screen share via getDisplayMedia...');
      // Plain boolean for display-capture audio. A constraints object
      // ({ echoCancellation: true }) is documented to make some browsers
      // silently DROP the track — and AEC is meaningless for system audio
      // (it's not a mic). The picker checkbox decides the final track.
      const stream = await navigator.mediaDevices.getDisplayMedia({
        video: { width: { ideal: 1920 }, height: { ideal: 1080 }, frameRate: { ideal: 60 } },
        audio: includeAudio,
        // Chrome/Edge-only: pre-request SYSTEM audio for screen/window
        // shares (the picker checkbox still decides the final track).
        // Ignored by Firefox/Safari. On Linux it only does anything when
        // Chrome was launched with --enable-features=PulseaudioLoopbackForScreenShare.
        systemAudio: includeAudio ? ('include' as const) : ('exclude' as const),
      } as DisplayMediaStreamOptions);

      this.screenStream = stream;
      this.eventBus.emit(Events.SCREEN_SHARE_STARTED, { stream });
      console.log(
        `[MEDIA] Screen share tracks: ${stream.getTracks().map((t) => t.kind).join(', ') || 'none'}`
      );

      // Chrome (especially on macOS) may deliver the display-audio track a
      // beat AFTER the picker closes — or never. Don't warn instantly; wait,
      // and PeerManager attaches late tracks as they land.
      if (includeAudio && stream.getAudioTracks().length === 0) {
        window.setTimeout(() => {
          const kinds = stream.getTracks().map((t) => t.kind).join(', ') || 'none';
          console.log(`[MEDIA] Screen share tracks after wait: ${kinds}`);
          if (stream.getAudioTracks().length === 0) {
            this.eventBus.emit(Events.TOAST, {
              message: screenAudioHint(navigator.userAgent),
              type: 'info',
              duration: 6000,
            } as ToastOptions);
          }
        }, 1500);
      }

      const videoTrack = stream.getVideoTracks()[0];
      if (videoTrack) {
        videoTrack.addEventListener('ended', () => {
          console.log('[MEDIA] Screen share stopped by user via browser UI');
          this.stopScreenShare();
        });
      }
      return stream;
    } catch (err) {
      console.error('[MEDIA] Screen share failed:', err);
      const denied = err instanceof Error && err.name === 'NotAllowedError';
      this.eventBus.emit(Events.TOAST, {
        message: denied ? 'Screen share canceled.' : 'Could not start screen share.',
        type: denied ? 'info' : 'error',
        duration: 3000,
      } as ToastOptions);
      return null;
    }
  }

  public stopScreenShare(): void {
    if (this.screenStream) {
      console.log('[MEDIA] Stopping screen share...');
      this.screenStream.getTracks().forEach((t) => t.stop());
      this.screenStream = null;
      this.eventBus.emit(Events.SCREEN_SHARE_STOPPED, {});
    }
  }

  /**
   * Cap the shared screen to the user-chosen resolution/FPS (PERF-AUDIT P1).
   * The browser picker delivers the display's NATIVE resolution (often 4K);
   * `max` caps are what actually force the encoder down — `ideal` alone
   * (used at capture time) is only a preference. `null` axes are left
   * untouched ("source native").
   */
  public async applyScreenShareConstraints(quality: ScreenShareQuality): Promise<void> {
    const track = this.screenStream?.getVideoTracks()[0];
    if (!track || typeof track.applyConstraints !== 'function') return;

    const constraints: MediaTrackConstraints = {};
    if (quality.width && quality.height) {
      constraints.width = { ideal: quality.width, max: quality.width };
      constraints.height = { ideal: quality.height, max: quality.height };
    }
    if (quality.frameRate) {
      constraints.frameRate = { ideal: quality.frameRate, max: quality.frameRate };
    }

    try {
      await track.applyConstraints(constraints);
      const applied = track.getSettings?.();
      console.log('[MEDIA] Screen share constraints applied:', JSON.stringify(constraints));
      if (applied && typeof applied === 'object') {
        console.log(
          `[MEDIA] Now capturing at ${applied.width}x${applied.height} @ ${applied.frameRate}fps`
        );
      }
    } catch (err) {
      console.warn('[MEDIA] Could not apply screen share quality:', err);
      this.eventBus.emit(Events.TOAST, {
        message: 'Your browser refused that resolution/FPS — continuing with the source default.',
        type: 'warning',
        duration: 4000,
      } as ToastOptions);
    }
  }

  public async switchMicrophone(deviceId: string): Promise<void> {
    if (!this.localStream) return;
    try {
      console.log(`[MEDIA] Switching mic to device ${deviceId}...`);
      const newStream = await navigator.mediaDevices.getUserMedia({
        audio: { deviceId: { exact: deviceId } },
      });
      const newTrack = newStream.getAudioTracks()[0];
      if (newTrack) {
        const oldTrack = this.localStream.getAudioTracks()[0];
        if (oldTrack) {
          this.localStream.removeTrack(oldTrack);
          oldTrack.stop();
        }
        this.localStream.addTrack(newTrack);
        this.eventBus.emit('media-track-replaced', { kind: 'audio', track: newTrack });
      }
    } catch (err) {
      console.error('[MEDIA] Failed to switch mic:', err);
    }
  }

  public async switchCamera(deviceId: string): Promise<void> {
    if (!this.localStream) return;
    try {
      console.log(`[MEDIA] Switching camera to device ${deviceId}...`);
      const newStream = await navigator.mediaDevices.getUserMedia({
        video: { deviceId: { exact: deviceId } },
      });
      const newTrack = newStream.getVideoTracks()[0];
      if (newTrack) {
        const oldTrack = this.localStream.getVideoTracks()[0];
        if (oldTrack) {
          this.localStream.removeTrack(oldTrack);
          oldTrack.stop();
        }
        this.localStream.addTrack(newTrack);
        this.eventBus.emit('media-track-replaced', { kind: 'video', track: newTrack });
      }
    } catch (err) {
      console.error('[MEDIA] Failed to switch camera:', err);
    }
  }

  public async getDevices(): Promise<MediaDevices> {
    try {
      if (!navigator.mediaDevices || !navigator.mediaDevices.enumerateDevices) {
        return { mics: [], cams: [], speakers: [] };
      }
      const devices = await navigator.mediaDevices.enumerateDevices();
      return {
        mics: devices.filter((d) => d.kind === 'audioinput'),
        cams: devices.filter((d) => d.kind === 'videoinput'),
        speakers: devices.filter((d) => d.kind === 'audiooutput'),
      };
    } catch (err) {
      console.error('[MEDIA] Failed to enumerate devices:', err);
      return { mics: [], cams: [], speakers: [] };
    }
  }

  public stopAllMedia(): void {
    console.log('[MEDIA] Stopping all media tracks...');
    if (this.localStream) {
      this.localStream.getTracks().forEach((t) => t.stop());
      this.localStream = null;
    }
    if (this.screenStream) {
      this.screenStream.getTracks().forEach((t) => t.stop());
      this.screenStream = null;
    }
  }
}
