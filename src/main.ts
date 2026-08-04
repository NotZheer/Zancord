import { eventBus } from './core/EventBus';
import { RoomManager } from './core/RoomManager';
import { PeerManager } from './core/PeerManager';
import { MediaManager } from './core/MediaManager';
import { AudioProcessor } from './core/AudioProcessor';
import { RemoteAudioMeter } from './core/RemoteAudioMeter';
import { UIRenderer } from './ui/UIRenderer';
import { ChatManager } from './ui/ChatManager';
import { ToastManager } from './ui/ToastManager';
import { ShareQualityModal } from './ui/ShareQualityModal';
import { Events, PeerInfo, ToastOptions } from './types';
import { resolveRoomId, buildInviteUrl } from './utils/room';
import { createLogBuffer } from './utils/logBuffer';

// Debug Log Interceptor — capped ring buffer so long calls don't grow memory
// without bound (PERF-AUDIT P4).
const logBuffer = createLogBuffer(500);
(window as any).__ZANCORD_LOGS__ = logBuffer.entries();
const originalLog = console.log;
const originalWarn = console.warn;
const originalError = console.error;

console.log = (...args: any[]) => {
  logBuffer.push(`[LOG ${new Date().toISOString()}] ${args.join(' ')}`);
  originalLog.apply(console, args);
};
console.warn = (...args: any[]) => {
  logBuffer.push(`[WARN ${new Date().toISOString()}] ${args.join(' ')}`);
  originalWarn.apply(console, args);
};
console.error = (...args: any[]) => {
  logBuffer.push(`[ERR ${new Date().toISOString()}] ${args.join(' ')}`);
  originalError.apply(console, args);
};

interface HotkeyBinding {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

interface HotkeyConfig {
  toggleMic: HotkeyBinding | null;
  toggleCam: HotkeyBinding | null;
  toggleDeafen: HotkeyBinding | null;
}

function formatHotkey(b: HotkeyBinding): string {
  const parts: string[] = [];
  if (b.metaKey) parts.push('⌘');
  if (b.ctrlKey) parts.push('Ctrl');
  if (b.altKey) parts.push('Alt');
  if (b.shiftKey) parts.push('Shift');
  parts.push(b.key.length === 1 ? b.key.toUpperCase() : b.key);
  return parts.join('+');
}

function matchesHotkey(e: KeyboardEvent, b: HotkeyBinding): boolean {
  return e.key.toLowerCase() === b.key.toLowerCase()
    && e.ctrlKey === b.ctrlKey
    && e.shiftKey === b.shiftKey
    && e.altKey === b.altKey
    && e.metaKey === b.metaKey;
}

class App {
  private roomManager: RoomManager;
  private peerManager: PeerManager;
  private mediaManager: MediaManager;
  private audioProcessor: AudioProcessor;
  private remoteAudioMeter: RemoteAudioMeter;
  private uiRenderer: UIRenderer;
  private chatManager: ChatManager;
  private shareQualityModal: ShareQualityModal;

  private roomId: string;
  private username: string;
  private tailscaleIp: string = '127.0.0.1';
  private isMuted: boolean = false;
  private isCamOff: boolean = false;
  private isDeafened: boolean = false;
  private isScreenSharing: boolean = false;
  private shareAudio: boolean = false;
  private hotkeys: HotkeyConfig = { toggleMic: null, toggleCam: null, toggleDeafen: null };

  constructor() {
    console.log('[ZANCORD v2] Initializing application...');

    // 1. DOM Element references
    const callGridEl = document.getElementById('call-grid')!;
    const toastContainerEl = document.getElementById('toast-container')!;
    const chatMessagesEl = document.getElementById('chat-messages');
    const chatFormEl = document.getElementById('chat-form') as HTMLFormElement | null;
    const chatBadgeEl = document.getElementById('chat-unread-badge');

    // 2. Instantiate core modules
    this.roomManager = new RoomManager(eventBus);
    this.peerManager = new PeerManager(eventBus);
    this.mediaManager = new MediaManager(eventBus);
    this.audioProcessor = new AudioProcessor(eventBus);
    this.remoteAudioMeter = new RemoteAudioMeter(eventBus);
    this.uiRenderer = new UIRenderer(callGridEl, eventBus);
    new ToastManager(toastContainerEl, eventBus);
    this.shareQualityModal = new ShareQualityModal();
    this.chatManager =
      chatMessagesEl && chatFormEl ? new ChatManager(eventBus, chatMessagesEl, chatFormEl, chatBadgeEl) : (null as unknown as ChatManager);

    // 3. Determine Room ID (shared default so installed PWAs connect
    //    instantly; #room= links override) and Saved Username
    this.roomId = resolveRoomId(window.location.hash);
    this.username = localStorage.getItem('zancord_username') || 'User';

    const nameInput = document.getElementById('input-display-name') as HTMLInputElement;
    if (nameInput) nameInput.value = this.username;
  }

  public async init(): Promise<void> {
    this.setupUIBindings();
    this.setupEventForwarding();
    this.setupWakeLock();
    await this.detectTailscaleIP();

    // 4. Initialize Local Media
    try {
      const rawStream = await this.mediaManager.initLocalMedia(true, true);
      const processedStream = this.audioProcessor.processStream(rawStream);

      this.peerManager.setLocalStream(processedStream);
      this.uiRenderer.createLocalCard(processedStream, this.username);
      eventBus.emit(Events.LOCAL_MEDIA_READY, { stream: processedStream });
    } catch (err) {
      console.error('[APP] Local media initialization failed:', err);
      eventBus.emit(Events.TOAST, {
        message: 'Could not access camera or microphone.',
        type: 'error',
        duration: 5000,
      } as ToastOptions);
    }

    // Populate media devices dropdowns
    await this.populateMediaDevices();

    // 5. Connect to Signaling Server & Join Room
    this.roomManager.connect();
    this.roomManager.joinRoom(this.roomId, this.username);
  }

  private setupEventForwarding(): void {
    // Local socket id → PeerManager for polite/impolite negotiation roles
    eventBus.on<{ socketId: string }>(Events.SOCKET_CONNECTED, ({ socketId }) => {
      this.peerManager.setLocalId(socketId);
    });

    // Connectivity feedback (U6)
    eventBus.on(Events.SOCKET_DISCONNECTED, () => {
      eventBus.emit(Events.TOAST, {
        message: 'Signaling connection lost. Reconnecting…',
        type: 'warning',
        duration: 4000,
      } as ToastOptions);
    });

    // Forward PeerManager RTC signal send requests to RoomManager
    eventBus.on<{ targetId: string; signal: any }>('rtc-send-signal', ({ targetId, signal }) => {
      this.roomManager.sendSignal(targetId, signal);
    });

    // Forward MediaManager track replacement to PeerManager
    eventBus.on<{ kind: 'audio' | 'video'; track: MediaStreamTrack }>('media-track-replaced', ({ kind, track }) => {
      this.peerManager.replaceTrack(kind, track);
    });

    // Chat Message Sent -> RoomManager
    eventBus.on<{ text: string }>(Events.CHAT_MESSAGE_SENT, ({ text }) => {
      this.roomManager.sendChatMessage(text);
    });

    // Chat Message Deleted -> RoomManager
    eventBus.on<{ id: string }>(Events.CHAT_MESSAGE_DELETED, ({ id }) => {
      this.roomManager.sendDeleteChatMessage(id);
    });

    // Chat Cleared -> RoomManager
    eventBus.on(Events.CHAT_CLEARED, () => {
      this.roomManager.sendClearChat();
    });

    // Peer Joined -> UI & Toast
    eventBus.on<PeerInfo>(Events.PEER_JOINED, (peer) => {
      this.uiRenderer.createPeerCard(peer.id, peer.username, peer);
      eventBus.emit(Events.TOAST, {
        message: `${peer.username} joined the room.`,
        type: 'info',
        duration: 3000,
      } as ToastOptions);
    });

    // Peer Left -> UI & Toast
    eventBus.on<{ peerId: string }>(Events.PEER_LEFT, ({ peerId }) => {
      const peer = this.peerManager.peers.get(peerId);
      const name = peer ? peer.username : 'A peer';
      this.uiRenderer.removePeerCard(peerId);
      this.remoteAudioMeter.detach(peerId);
      eventBus.emit(Events.TOAST, {
        message: `${name} left the room.`,
        type: 'warning',
        duration: 3000,
      } as ToastOptions);
    });

    // Peer Stream Added -> UI (+ remote audio meter for the speaking ring)
    eventBus.on<{ peerId: string; stream: MediaStream }>(Events.PEER_STREAM_ADDED, ({ peerId, stream }) => {
      this.uiRenderer.setPeerStream(peerId, stream);
      this.remoteAudioMeter.attach(peerId, stream);
    });

    // Peer Screen Stream Added -> UI (Discord-style separate screen share card)
    eventBus.on<{ peerId: string; username: string; stream: MediaStream }>(
      'peer-screen-stream-added',
      ({ peerId, username, stream }) => {
        this.uiRenderer.createPeerCard(peerId, username);
        this.uiRenderer.setPeerStream(peerId, stream);
        this.remoteAudioMeter.attach(peerId, stream);
      }
    );

    // Peer Screen Stream Removed -> UI cleanup (U10)
    eventBus.on<{ peerId: string }>('peer-screen-stream-removed', ({ peerId }) => {
      this.uiRenderer.removePeerCard(peerId);
      this.remoteAudioMeter.detach(peerId);
    });

    eventBus.on(Events.SCREEN_SHARE_STOPPED, () => {
      this.uiRenderer.removePeerCard('local-screen');
      this.peerManager.removeScreenTrack();
      this.isScreenSharing = false;
      const btnScreen = document.getElementById('btn-toggle-screen');
      btnScreen?.classList.remove('active');
      this.roomManager.emitStateChange({ isScreenSharing: false });
    });

    // Peer State Changed -> UI
    eventBus.on<{ peerId: string; state: any }>(Events.PEER_STATE_CHANGED, ({ peerId, state }) => {
      if (state.isScreenSharing === false) {
        this.uiRenderer.removePeerCard(`${peerId}-screen`);
      }
      this.uiRenderer.updatePeerState(peerId, state);
    });

    // Audio Level -> UI ring glow
    eventBus.on<{ target: string; level: number }>(Events.AUDIO_LEVEL, ({ target, level }) => {
      this.uiRenderer.setAudioLevel(target, level);
    });

    // Connection State -> UI dot
    eventBus.on<{ peerId: string; state: RTCIceConnectionState }>(
      Events.CONNECTION_STATE_CHANGED,
      ({ peerId, state }) => {
        this.uiRenderer.setConnectionQuality(peerId, state);
      }
    );
  }

  private setupUIBindings(): void {
    this.setupInactivityAutoFade();

    // Dock Controls
    const btnMic = document.getElementById('btn-toggle-mic');
    btnMic?.addEventListener('click', () => this.toggleMic(btnMic));

    const btnCam = document.getElementById('btn-toggle-cam');
    btnCam?.addEventListener('click', () => this.toggleCam(btnCam));

    const btnScreen = document.getElementById('btn-toggle-screen');
    btnScreen?.addEventListener('click', () => this.toggleScreenShare(btnScreen));

    const btnDeafen = document.getElementById('btn-toggle-deafen');
    btnDeafen?.addEventListener('click', () => this.toggleDeafen(btnDeafen));

    const btnLeave = document.getElementById('btn-leave');
    btnLeave?.addEventListener('click', () => this.leaveCall());

    // Top Nav Controls
    const btnFullscreen = document.getElementById('btn-fullscreen');
    btnFullscreen?.addEventListener('click', () => this.toggleFullscreen());

    const btnSettings = document.getElementById('btn-toggle-settings');
    const btnCloseSettings = document.getElementById('btn-close-settings');
    const settingsSidebar = document.getElementById('settings-sidebar');

    btnSettings?.addEventListener('click', () => settingsSidebar?.classList.toggle('open'));
    btnCloseSettings?.addEventListener('click', () => settingsSidebar?.classList.remove('open'));

    // Chat Drawer (U1)
    const chatDrawer = document.getElementById('chat-drawer');
    const btnChat = document.getElementById('btn-toggle-chat');
    const btnCloseChat = document.getElementById('btn-close-chat');
    btnChat?.addEventListener('click', () => {
      const open = chatDrawer?.classList.toggle('open') ?? false;
      this.chatManager.setDrawerOpen(open);
    });
    btnCloseChat?.addEventListener('click', () => {
      chatDrawer?.classList.remove('open');
      this.chatManager.setDrawerOpen(false);
    });

    const btnCopyInvite = document.getElementById('btn-copy-invite');
    btnCopyInvite?.addEventListener('click', () => this.copyInviteLink());

    // Settings Controls
    const inputName = document.getElementById('input-display-name') as HTMLInputElement;
    inputName?.addEventListener('change', () => {
      this.username = inputName.value.trim() || 'User';
      localStorage.setItem('zancord_username', this.username);
      this.uiRenderer.updatePeerState('local', { username: `${this.username} (You)` });
      this.roomManager.emitStateChange({ username: this.username });
    });

    const selectMic = document.getElementById('select-mic-device') as HTMLSelectElement;
    selectMic?.addEventListener('change', async () => {
      localStorage.setItem('zancord_mic_device', selectMic.value);
      await this.mediaManager.switchMicrophone(selectMic.value);
      const rawStream = this.mediaManager.getLocalStream();
      if (rawStream) {
        const processedStream = this.audioProcessor.processStream(rawStream);
        const newAudioTrack = processedStream.getAudioTracks()[0];
        if (newAudioTrack) {
          this.peerManager.replaceTrack('audio', newAudioTrack);
        }
      }
    });

    const selectCam = document.getElementById('select-cam-device') as HTMLSelectElement;
    selectCam?.addEventListener('change', () => {
      localStorage.setItem('zancord_cam_device', selectCam.value);
      this.mediaManager.switchCamera(selectCam.value);
    });

    const selectSpeaker = document.getElementById('select-speaker-device') as HTMLSelectElement;
    selectSpeaker?.addEventListener('change', () => {
      localStorage.setItem('zancord_speaker_device', selectSpeaker.value);
      this.setSpeakerDevice(selectSpeaker.value);
    });

    // Restore saved noise gate settings
    const savedNoiseEnabled = localStorage.getItem('zancord_noise_enabled') !== 'false';
    const savedNoiseThreshold = parseInt(localStorage.getItem('zancord_noise_threshold') || '-45');
    this.audioProcessor.setEnabled(savedNoiseEnabled);
    this.audioProcessor.setNoiseGateThreshold(savedNoiseThreshold);

    const toggleNoise = document.getElementById('toggle-noise-suppression') as HTMLInputElement;
    if (toggleNoise) toggleNoise.checked = savedNoiseEnabled;
    toggleNoise?.addEventListener('change', () => {
      localStorage.setItem('zancord_noise_enabled', toggleNoise.checked ? 'true' : 'false');
      this.audioProcessor.setEnabled(toggleNoise.checked);
    });

    // Screen share audio toggle (persisted)
    const savedShareAudio = localStorage.getItem('zancord_share_audio') === 'true';
    this.shareAudio = savedShareAudio;
    const toggleShareAudio = document.getElementById('toggle-share-audio') as HTMLInputElement;
    if (toggleShareAudio) toggleShareAudio.checked = savedShareAudio;
    toggleShareAudio?.addEventListener('change', () => {
      this.shareAudio = toggleShareAudio.checked;
      localStorage.setItem('zancord_share_audio', toggleShareAudio.checked ? 'true' : 'false');
    });

    const rangeNoise = document.getElementById('input-noise-sensitivity') as HTMLInputElement;
    const noiseReadout = document.getElementById('sensitivity-db-value');
    if (rangeNoise) rangeNoise.value = savedNoiseThreshold.toString();
    if (noiseReadout) noiseReadout.textContent = `${savedNoiseThreshold} dB`;

    rangeNoise?.addEventListener('input', () => {
      const val = parseInt(rangeNoise.value);
      localStorage.setItem('zancord_noise_threshold', val.toString());
      if (noiseReadout) noiseReadout.textContent = `${val} dB`;
      this.audioProcessor.setNoiseGateThreshold(val);
    });

    // ── Hotkeys ──
    this.loadHotkeys();
    this.setupHotkeyRecording();
    this.setupGlobalHotkeyListener();
  }

  private toggleMic(btn: HTMLElement): void {
    this.isMuted = !this.isMuted;
    this.mediaManager.toggleMicrophone(!this.isMuted);
    btn.classList.toggle('off', this.isMuted);
    btn.querySelector('i')?.setAttribute('class', this.isMuted ? 'fa-solid fa-microphone-slash' : 'fa-solid fa-microphone');
    this.uiRenderer.updatePeerState('local', { isMuted: this.isMuted });
    this.roomManager.emitStateChange({ isMuted: this.isMuted });
  }

  private toggleCam(btn: HTMLElement): void {
    this.isCamOff = !this.isCamOff;
    this.mediaManager.toggleCamera(!this.isCamOff);
    btn.classList.toggle('off', this.isCamOff);
    btn.querySelector('i')?.setAttribute('class', this.isCamOff ? 'fa-solid fa-video-slash' : 'fa-solid fa-video');
    this.uiRenderer.updatePeerState('local', { isCamOff: this.isCamOff });
    this.roomManager.emitStateChange({ isCamOff: this.isCamOff });
  }

  private async toggleScreenShare(btn: HTMLElement): Promise<void> {
    if (this.isScreenSharing) {
      this.mediaManager.stopScreenShare();
      this.peerManager.removeScreenTrack();
      this.uiRenderer.removePeerCard('local-screen');
      this.isScreenSharing = false;
      btn.classList.remove('active');
      this.roomManager.emitStateChange({ isScreenSharing: false });
    } else {
      const screenStream = await this.mediaManager.startScreenShare(this.shareAudio);
      if (!screenStream || !screenStream.getVideoTracks()[0]) return;

      // PERF-AUDIT P1: the picker delivers the display's NATIVE resolution;
      // let the user cap resolution/FPS before peers receive it. Cancel →
      // tear the capture back down.
      const quality = await this.shareQualityModal.open(screenStream, this.shareAudio);
      if (!quality) {
        this.mediaManager.stopScreenShare();
        return;
      }
      await this.mediaManager.applyScreenShareConstraints(quality);

      this.uiRenderer.createPeerCard('local-screen', `${this.username}'s Screen`);
      this.uiRenderer.setPeerStream('local-screen', screenStream);
      console.log(
        `[APP] Sharing screen with tracks: ${screenStream.getTracks().map((t) => t.kind).join(', ') || 'none'}`
      );
      this.peerManager.addScreenTrack(screenStream);
      this.isScreenSharing = true;
      btn.classList.add('active');
      this.roomManager.emitStateChange({ isScreenSharing: true });
    }
  }

  private toggleDeafen(btn: HTMLElement): void {
    this.isDeafened = !this.isDeafened;
    btn.classList.toggle('off', this.isDeafened);
    btn.querySelector('i')?.setAttribute('class', this.isDeafened ? 'fa-solid fa-ear-deaf' : 'fa-solid fa-headphones');

    // Deafen state is enforced by the renderer for current AND future cards (U2)
    this.uiRenderer.setDeafened(this.isDeafened);

    eventBus.emit(Events.TOAST, {
      message: this.isDeafened ? 'Deafened: Remote audio muted.' : 'Undeafened: Remote audio unmuted.',
      type: 'info',
      duration: 2000,
    } as ToastOptions);
  }

  private leaveCall(): void {
    console.log('[APP] Leaving call...');
    this.peerManager.closeAllConnections();
    this.mediaManager.stopAllMedia();
    this.audioProcessor.destroy();
    this.remoteAudioMeter.detachAll();
    this.roomManager.leaveRoom();

    eventBus.emit(Events.TOAST, {
      message: 'You left the room.',
      type: 'warning',
      duration: 3000,
    } as ToastOptions);

    setTimeout(() => {
      window.location.reload();
    }, 1000);
  }



  private copyInviteLink(): void {
    // Production: same-origin (Tailscale Serve / direct IP). Dev: IP + :3443 fallback.
    const inviteUrl = buildInviteUrl(window.location.origin, window.location.hostname, this.tailscaleIp, this.roomId);
    navigator.clipboard
      .writeText(inviteUrl)
      .then(() => {
        eventBus.emit(Events.TOAST, {
          message: 'Invite link copied to clipboard!',
          type: 'success',
          duration: 3000,
        } as ToastOptions);
      })
      .catch((err) => {
        console.error('Failed to copy invite link:', err);
      });
  }

  private async populateMediaDevices(): Promise<void> {
    const devices = await this.mediaManager.getDevices();

    const savedMic = localStorage.getItem('zancord_mic_device');
    const savedCam = localStorage.getItem('zancord_cam_device');
    const savedSpeaker = localStorage.getItem('zancord_speaker_device');

    const selectMic = document.getElementById('select-mic-device') as HTMLSelectElement;
    if (selectMic && devices.mics.length > 0) {
      selectMic.innerHTML = devices.mics
        .map((d, i) => `<option value="${d.deviceId}">${d.label || `Microphone ${i + 1}`}</option>`)
        .join('');
      if (savedMic && devices.mics.some((d) => d.deviceId === savedMic)) {
        selectMic.value = savedMic;
      }
    }

    const selectCam = document.getElementById('select-cam-device') as HTMLSelectElement;
    if (selectCam && devices.cams.length > 0) {
      selectCam.innerHTML = devices.cams
        .map((d, i) => `<option value="${d.deviceId}">${d.label || `Camera ${i + 1}`}</option>`)
        .join('');
      if (savedCam && devices.cams.some((d) => d.deviceId === savedCam)) {
        selectCam.value = savedCam;
      }
    }

    const selectSpeaker = document.getElementById('select-speaker-device') as HTMLSelectElement;
    if (selectSpeaker && devices.speakers.length > 0) {
      selectSpeaker.innerHTML = devices.speakers
        .map((d, i) => `<option value="${d.deviceId}">${d.label || `Speaker ${i + 1}`}</option>`)
        .join('');
      if (savedSpeaker && devices.speakers.some((d) => d.deviceId === savedSpeaker)) {
        selectSpeaker.value = savedSpeaker;
        this.setSpeakerDevice(savedSpeaker);
      }
    }
  }

  private async setSpeakerDevice(deviceId: string): Promise<void> {
    const videos = document.querySelectorAll('video');
    for (const v of Array.from(videos)) {
      if ('setSinkId' in v) {
        try {
          await (v as any).setSinkId(deviceId);
        } catch (err) {
          console.warn('Failed to setSinkId on video:', err);
        }
      }
    }
  }

  private async detectTailscaleIP(): Promise<void> {
    const badgeEl = document.getElementById('tailscale-ip-display');

    // PWA: the host we were served from IS the Tailscale host.
    this.tailscaleIp = window.location.hostname || '127.0.0.1';
    if (badgeEl) badgeEl.textContent = `Host: ${this.tailscaleIp}`;
  }

  private setupWakeLock(): void {
    // Keep the screen awake during calls (P4) — feature-detected, best-effort.
    const nav = navigator as Navigator & { wakeLock?: { request: (type: 'screen') => Promise<unknown> } };
    if (!nav.wakeLock) return;

    let lock: unknown = null;
    const acquire = async () => {
      try {
        lock = await nav.wakeLock!.request('screen');
      } catch {
        // Wake lock unavailable (e.g. battery saver) — ignore.
      }
    };

    window.addEventListener('pointerdown', acquire, { once: true });
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'visible' && !lock) {
        acquire();
      }
    });
  }

  private setupInactivityAutoFade(): void {
    let timer: number | null = null;
    const hideControls = () => {
      document.body.classList.add('user-inactive');
    };
    const showControls = () => {
      document.body.classList.remove('user-inactive');
      if (timer) clearTimeout(timer);
      timer = window.setTimeout(hideControls, 3000);
    };

    window.addEventListener('mousemove', showControls);
    window.addEventListener('mousedown', showControls);
    window.addEventListener('keydown', showControls);
    window.addEventListener('touchstart', showControls);
    showControls();
  }

  private toggleFullscreen(): void {
    if (!document.fullscreenElement) {
      document.documentElement.requestFullscreen().catch((err) => {
        console.warn('Error attempting to enable fullscreen:', err);
      });
    } else {
      document.exitFullscreen().catch((err) => {
        console.warn('Error attempting to exit fullscreen:', err);
      });
    }
  }

  private loadHotkeys(): void {
    try {
      const raw = localStorage.getItem('zancord_hotkeys');
      if (raw) {
        const parsed = JSON.parse(raw) as HotkeyConfig;
        this.hotkeys = parsed;
        // Update button labels
        for (const [action, binding] of Object.entries(parsed)) {
          if (binding) {
            const btn = document.querySelector(`.hotkey-btn[data-action="${action}"]`) as HTMLElement;
            if (btn) btn.textContent = formatHotkey(binding);
          }
        }
      }
    } catch { /* ignore corrupt data */ }
  }

  private setupHotkeyRecording(): void {
    const buttons = document.querySelectorAll<HTMLButtonElement>('.hotkey-btn');
    buttons.forEach((btn) => {
      btn.addEventListener('click', () => {
        // If already recording on another button, cancel it
        document.querySelectorAll('.hotkey-btn.recording').forEach((b) => b.classList.remove('recording'));

        const action = btn.dataset.action as keyof HotkeyConfig;

        // Re-click while recording cancels
        if (btn.classList.contains('recording')) {
          btn.classList.remove('recording');
          return;
        }

        btn.classList.add('recording');
        btn.textContent = 'Press key...';

        const handler = (e: KeyboardEvent) => {
          e.preventDefault();
          e.stopPropagation();

          // Ignore bare modifier keys
          if (['Control', 'Shift', 'Alt', 'Meta'].includes(e.key)) return;

          const binding: HotkeyBinding = {
            key: e.key,
            ctrlKey: e.ctrlKey,
            shiftKey: e.shiftKey,
            altKey: e.altKey,
            metaKey: e.metaKey,
          };

          this.hotkeys[action] = binding;
          localStorage.setItem('zancord_hotkeys', JSON.stringify(this.hotkeys));
          btn.textContent = formatHotkey(binding);
          btn.classList.remove('recording');
          window.removeEventListener('keydown', handler, true);
        };

        window.addEventListener('keydown', handler, true);
      });

      // Right-click to clear a binding
      btn.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        const action = btn.dataset.action as keyof HotkeyConfig;
        this.hotkeys[action] = null;
        localStorage.setItem('zancord_hotkeys', JSON.stringify(this.hotkeys));
        btn.textContent = 'None';
        btn.classList.remove('recording');
      });
    });
  }

  private setupGlobalHotkeyListener(): void {
    window.addEventListener('keydown', (e: KeyboardEvent) => {
      // Don't fire hotkeys when typing in inputs, textareas, or selects
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;

      // Don't fire while recording a new hotkey
      if (document.querySelector('.hotkey-btn.recording')) return;

      const btnMic = document.getElementById('btn-toggle-mic');
      const btnCam = document.getElementById('btn-toggle-cam');
      const btnDeafen = document.getElementById('btn-toggle-deafen');

      if (this.hotkeys.toggleMic && btnMic && matchesHotkey(e, this.hotkeys.toggleMic)) {
        e.preventDefault();
        this.toggleMic(btnMic);
      } else if (this.hotkeys.toggleCam && btnCam && matchesHotkey(e, this.hotkeys.toggleCam)) {
        e.preventDefault();
        this.toggleCam(btnCam);
      } else if (this.hotkeys.toggleDeafen && btnDeafen && matchesHotkey(e, this.hotkeys.toggleDeafen)) {
        e.preventDefault();
        this.toggleDeafen(btnDeafen);
      }
    });
  }
}

// Bootstrap app on DOMReady
window.addEventListener('DOMContentLoaded', () => {
  const app = new App();
  app.init().catch((err) => console.error('[APP] Initialization error:', err));

  if ('serviceWorker' in navigator) {
    navigator.serviceWorker.register('/sw.js').catch((err) => console.warn('[PWA] Service worker registration:', err));
  }
});
