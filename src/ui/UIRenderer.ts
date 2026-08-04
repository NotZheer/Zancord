import { EventBus } from '../core/EventBus';
import { PeerInfo } from '../types';

interface PeerAudioState {
  muted: boolean;
  volume: number;
}

interface PipGeometry {
  left: number;
  top: number;
  width: number;
}

export class UIRenderer {
  private container: HTMLElement;
  private localCard: HTMLElement | null = null;
  private cards: Map<string, HTMLElement> = new Map();
  private spotlightId: string | null = null;
  private cameraStrip: HTMLElement;
  private deafened: boolean = false;
  private peerAudio: Map<string, PeerAudioState> = new Map();
  private audioPopover: HTMLElement;
  private audioPopoverPeerId: string | null = null;
  private audioPopoverTrigger: HTMLElement | null = null;
  private pipPositions: Map<string, PipGeometry> = new Map();
  private resizeObserver: ResizeObserver | null = null;

  constructor(container: HTMLElement, _eventBus: EventBus) {
    this.container = container;
    this.cameraStrip = this.createCameraStrip();
    this.audioPopover = this.createAudioPopover();
    this.container.appendChild(this.audioPopover);

    if (typeof ResizeObserver !== 'undefined') {
      this.resizeObserver = new ResizeObserver(() => {
        this.updateGalleryLayout();
        if (this.isPipMode()) this.layoutPipTiles();
      });
      this.resizeObserver.observe(this.container);
    }

    document.addEventListener('pointerdown', (event) => {
      if (!this.audioPopover.hidden && event.target instanceof Node && !this.audioPopover.contains(event.target)) {
        this.closeAudioPopover();
      }
    });
    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && !this.audioPopover.hidden) {
        event.preventDefault();
        this.closeAudioPopover(true);
      }
    });
  }

  /**
   * Camera container. When nothing is pinned it stays hidden; when a camera
   * is pinned it's a simple fixed thumbnail strip along the bottom; when a
   * screen share is pinned the same cameras become freely draggable/resizable
   * PiP bubbles (see wirePipHandles / layoutPipTiles). No orientation toggle:
   * bubbles are placed freely by the user and remember their position.
   */
  private createCameraStrip(): HTMLElement {
    const strip = document.createElement('div');
    strip.className = 'camera-strip';
    strip.id = 'camera-strip';
    return strip;
  }

  private createAudioPopover(): HTMLElement {
    const popover = document.createElement('section');
    popover.className = 'stream-audio-popover';
    popover.hidden = true;
    popover.setAttribute('role', 'dialog');
    popover.setAttribute('aria-label', 'Stream audio controls');
    popover.innerHTML = `
      <div class="stream-audio-popover-header">
        <span class="stream-audio-label">STREAM AUDIO</span>
        <button type="button" class="stream-audio-close" aria-label="Close stream audio controls">
          <i class="fa-solid fa-xmark" aria-hidden="true"></i>
        </button>
      </div>
      <p class="stream-audio-name"></p>
      <button type="button" class="stream-audio-mute"></button>
      <label class="stream-audio-volume-label">
        <span>Volume</span>
        <output class="stream-audio-volume-output">100%</output>
        <input class="stream-audio-volume" type="range" min="0" max="100" step="1" value="100" aria-label="Stream volume">
      </label>
    `;

    const closeButton = popover.querySelector('.stream-audio-close') as HTMLButtonElement;
    const muteButton = popover.querySelector('.stream-audio-mute') as HTMLButtonElement;
    const volume = popover.querySelector('.stream-audio-volume') as HTMLInputElement;
    closeButton.addEventListener('click', () => this.closeAudioPopover(true));
    muteButton.addEventListener('click', () => {
      if (!this.audioPopoverPeerId) return;
      const state = this.getAudioState(this.audioPopoverPeerId);
      this.setPeerMuted(this.audioPopoverPeerId, !state.muted);
    });
    volume.addEventListener('input', () => {
      if (!this.audioPopoverPeerId) return;
      this.setPeerVolume(this.audioPopoverPeerId, Number(volume.value) / 100);
    });

    return popover;
  }

  private setAudioMuteButton(button: HTMLButtonElement, muted: boolean): void {
    button.replaceChildren();
    const icon = document.createElement('i');
    icon.className = muted ? 'fa-solid fa-volume-xmark' : 'fa-solid fa-volume-high';
    icon.setAttribute('aria-hidden', 'true');
    const label = document.createElement('span');
    label.textContent = muted ? 'Unmute audio' : 'Mute audio';
    button.append(icon, label);
    button.setAttribute('aria-pressed', String(muted));
  }

  private openAudioPopover(peerId: string, username: string, x: number, y: number, trigger: HTMLElement): void {
    const state = this.getAudioState(peerId);
    const name = this.audioPopover.querySelector('.stream-audio-name') as HTMLElement;
    const muteButton = this.audioPopover.querySelector('.stream-audio-mute') as HTMLButtonElement;
    const volume = this.audioPopover.querySelector('.stream-audio-volume') as HTMLInputElement;
    const output = this.audioPopover.querySelector('.stream-audio-volume-output') as HTMLOutputElement;
    name.textContent = username;
    this.setAudioMuteButton(muteButton, state.muted);
    volume.value = String(Math.round(state.volume * 100));
    output.value = `${Math.round(state.volume * 100)}%`;

    const margin = 8;
    const popoverWidth = 272;
    const popoverHeight = 184;
    this.audioPopover.style.left = `${Math.max(margin, Math.min(x, window.innerWidth - popoverWidth - margin))}px`;
    this.audioPopover.style.top = `${Math.max(margin, Math.min(y, window.innerHeight - popoverHeight - margin))}px`;
    this.audioPopover.hidden = false;
    this.audioPopoverPeerId = peerId;
    this.audioPopoverTrigger = trigger;
    muteButton.focus();
  }

  private updateAudioPopover(peerId: string): void {
    if (this.audioPopover.hidden || this.audioPopoverPeerId !== peerId) return;
    const state = this.getAudioState(peerId);
    const muteButton = this.audioPopover.querySelector('.stream-audio-mute') as HTMLButtonElement;
    const volume = this.audioPopover.querySelector('.stream-audio-volume') as HTMLInputElement;
    const output = this.audioPopover.querySelector('.stream-audio-volume-output') as HTMLOutputElement;
    this.setAudioMuteButton(muteButton, state.muted);
    volume.value = String(Math.round(state.volume * 100));
    output.value = `${Math.round(state.volume * 100)}%`;
  }

  private closeAudioPopover(restoreFocus = false): void {
    if (this.audioPopover.hidden) return;
    const trigger = this.audioPopoverTrigger;
    this.audioPopover.hidden = true;
    this.audioPopoverPeerId = null;
    this.audioPopoverTrigger = null;
    if (restoreFocus) trigger?.focus();
  }

  private updateGalleryLayout(): void {
    // The sharer never sees their own screen tile, so it must not claim a
    // grid slot or count toward the gallery geometry.
    const selfSharePinned = this.isSelfScreenShare(this.spotlightId);
    const count = this.cards.size - (this.cards.has('local-screen') ? 1 : 0);
    if ((this.spotlightId && !selfSharePinned) || count === 0) {
      this.container.style.removeProperty('--gallery-columns');
      this.container.style.removeProperty('--gallery-cell-width');
      this.container.style.removeProperty('--gallery-cell-height');
      return;
    }

    const bounds = this.container.getBoundingClientRect();
    if (bounds.width <= 0 || bounds.height <= 0) return;

    const columns = this.getGalleryColumns(count, bounds.width / bounds.height);
    const rows = Math.ceil(count / columns);
    const gap = 16;
    const availableWidth = bounds.width - gap * (columns - 1);
    const availableHeight = bounds.height - gap * (rows - 1);
    const cellWidth = Math.floor(Math.min(availableWidth / columns, (availableHeight / rows) * (16 / 9)) * 1000) / 1000;
    const cellHeight = cellWidth * (9 / 16);

    this.container.style.setProperty('--gallery-columns', String(columns));
    this.container.style.setProperty('--gallery-cell-width', `${cellWidth}px`);
    this.container.style.setProperty('--gallery-cell-height', `${cellHeight}px`);
  }

  private getGalleryColumns(count: number, viewportRatio: number): number {
    if (count === 1) return 1;
    if (viewportRatio >= 1.15) return count <= 4 ? 2 : 3;
    if (viewportRatio >= 0.75) return count === 2 ? 1 : 2;
    return count <= 3 ? 1 : 2;
  }

  // ---------------------------------------------------------------------
  // Screen-share PiP camera bubbles: only active while a screen share is
  // pinned. Regular camera pins stay simple (fixed strip, no drag/resize) —
  // matching Discord. This is the one place that DOES support free move +
  // resize, and only for cameras, never for the shared screen itself.
  // ---------------------------------------------------------------------

  private isScreenId(id: string): boolean {
    return id.includes('screen');
  }

  private isSelfScreenShare(id: string | null): boolean {
    return id === 'local-screen';
  }

  private isPipMode(): boolean {
    return !!this.spotlightId && this.isScreenId(this.spotlightId) && !this.isSelfScreenShare(this.spotlightId);
  }

  private getTileAspect(card: HTMLElement): number {
    return card.classList.contains('is-portrait') ? 9 / 16 : 16 / 9;
  }

  private clampPipGeometry(card: HTMLElement, geometry: PipGeometry): PipGeometry {
    const bounds = this.container.getBoundingClientRect();
    if (bounds.width <= 0 || bounds.height <= 0) return geometry;
    const aspect = this.getTileAspect(card);
    const margin = 12;
    const maxWidth = Math.max(96, bounds.width - margin * 2);
    const minWidth = Math.min(140, maxWidth);
    const width = Math.max(minWidth, Math.min(geometry.width, maxWidth));
    const height = width / aspect;
    const left = Math.max(margin, Math.min(geometry.left, bounds.width - width - margin));
    const top = Math.max(margin, Math.min(geometry.top, bounds.height - height - margin));
    return { left, top, width };
  }

  private applyPipGeometry(card: HTMLElement, geometry: PipGeometry): void {
    card.style.setProperty('--pip-left', `${Math.round(geometry.left)}px`);
    card.style.setProperty('--pip-top', `${Math.round(geometry.top)}px`);
    card.style.setProperty('--pip-width', `${Math.round(geometry.width)}px`);
  }

  private clearPipGeometry(card: HTMLElement): void {
    card.style.removeProperty('--pip-left');
    card.style.removeProperty('--pip-top');
    card.style.removeProperty('--pip-width');
    card.classList.remove('pip-dragging');
  }

  private defaultPipSlot(index: number, card: HTMLElement): PipGeometry {
    const bounds = this.container.getBoundingClientRect();
    const aspect = this.getTileAspect(card);
    const width = 208;
    const height = width / aspect;
    const margin = 16;
    const gap = 12;
    // Start stacked along the bottom edge; the user can then drag each
    // bubble anywhere they like and it remembers its position.
    return { left: margin + index * (width + gap), top: bounds.height - height - margin, width };
  }

  private layoutPipTiles(): void {
    let index = 0;
    this.cards.forEach((card, id) => {
      if (id === this.spotlightId) return;
      if (card.parentNode !== this.cameraStrip) return;
      let geometry = this.pipPositions.get(id);
      if (!geometry) {
        geometry = this.defaultPipSlot(index, card);
      }
      geometry = this.clampPipGeometry(card, geometry);
      this.pipPositions.set(id, geometry);
      this.applyPipGeometry(card, geometry);
      index++;
    });
  }

  private wirePipHandles(card: HTMLElement, peerId: string): void {
    const moveHandle = card.querySelector('.pip-move-handle') as HTMLButtonElement;
    const resizeHandle = card.querySelector('.pip-resize-handle') as HTMLButtonElement;
    // Grab anywhere on the bubble to move it freely. The move handle stays as
    // a keyboard-accessible alternative; the resize corner stays exclusive.
    card.addEventListener('pointerdown', (event) => {
      if ((event.target as HTMLElement).closest('.pip-move-handle, .pip-resize-handle')) return;
      if (card.parentNode !== this.cameraStrip) return;
      this.startPipDrag(event, card, peerId);
    });
    moveHandle.addEventListener('pointerdown', (event) => this.startPipDrag(event, card, peerId));
    moveHandle.addEventListener('keydown', (event) => this.nudgePipTile(event, card, peerId, false));
    resizeHandle.addEventListener('pointerdown', (event) => this.startPipResize(event, card, peerId));
    resizeHandle.addEventListener('keydown', (event) => this.nudgePipTile(event, card, peerId, true));
  }

  private startPipDrag(event: PointerEvent, card: HTMLElement, peerId: string): void {
    if (!this.isPipMode() || event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const handle = event.currentTarget as HTMLElement;
    const bounds = this.container.getBoundingClientRect();
    const rect = card.getBoundingClientRect();
    const startLeft = rect.left - bounds.left;
    const startTop = rect.top - bounds.top;
    const startX = event.clientX;
    const startY = event.clientY;
    const width = rect.width;
    handle.setPointerCapture(event.pointerId);
    card.classList.add('pip-dragging');

    const move = (moveEvent: PointerEvent) => {
      const geometry = this.clampPipGeometry(card, {
        left: startLeft + moveEvent.clientX - startX,
        top: startTop + moveEvent.clientY - startY,
        width,
      });
      this.pipPositions.set(peerId, geometry);
      this.applyPipGeometry(card, geometry);
    };
    const stop = (stopEvent: PointerEvent) => {
      try {
        handle.releasePointerCapture(stopEvent.pointerId);
      } catch {
        // capture may already be released
      }
      card.classList.remove('pip-dragging');
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', stop);
      handle.removeEventListener('pointercancel', stop);
    };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', stop);
    handle.addEventListener('pointercancel', stop);
  }

  private startPipResize(event: PointerEvent, card: HTMLElement, peerId: string): void {
    if (!this.isPipMode()) return;
    event.preventDefault();
    event.stopPropagation();
    const handle = event.currentTarget as HTMLElement;
    const bounds = this.container.getBoundingClientRect();
    const rect = card.getBoundingClientRect();
    const startLeft = rect.left - bounds.left;
    const startTop = rect.top - bounds.top;
    const startWidth = rect.width;
    const startX = event.clientX;
    handle.setPointerCapture(event.pointerId);

    const move = (moveEvent: PointerEvent) => {
      const geometry = this.clampPipGeometry(card, {
        left: startLeft,
        top: startTop,
        width: startWidth + (moveEvent.clientX - startX),
      });
      this.pipPositions.set(peerId, geometry);
      this.applyPipGeometry(card, geometry);
    };
    const stop = (stopEvent: PointerEvent) => {
      try {
        handle.releasePointerCapture(stopEvent.pointerId);
      } catch {
        // capture may already be released
      }
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', stop);
      handle.removeEventListener('pointercancel', stop);
    };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', stop);
    handle.addEventListener('pointercancel', stop);
  }

  private nudgePipTile(event: KeyboardEvent, card: HTMLElement, peerId: string, resize: boolean): void {
    if (!this.isPipMode()) return;
    const distance = event.shiftKey ? 40 : 12;
    let dx = 0;
    let dy = 0;
    if (event.key === 'ArrowLeft') dx = -distance;
    if (event.key === 'ArrowRight') dx = distance;
    if (event.key === 'ArrowUp') dy = -distance;
    if (event.key === 'ArrowDown') dy = distance;
    if (dx === 0 && dy === 0) return;

    event.preventDefault();
    event.stopPropagation();
    const current = this.pipPositions.get(peerId) || this.clampPipGeometry(card, this.defaultPipSlot(0, card));
    const geometry = resize
      ? this.clampPipGeometry(card, { left: current.left, top: current.top, width: current.width + dx + dy })
      : this.clampPipGeometry(card, { left: current.left + dx, top: current.top + dy, width: current.width });
    this.pipPositions.set(peerId, geometry);
    this.applyPipGeometry(card, geometry);
  }

  private getCardUsername(card: HTMLElement): string {
    return card.querySelector('.peer-name')?.textContent?.trim() || 'Participant';
  }

  private getInitials(username: string): string {
    const parts = username.trim().split(' ');
    if (parts.length >= 2) {
      return (parts[0][0] + parts[1][0]).toUpperCase();
    }
    return username.slice(0, 2).toUpperCase() || 'U';
  }

  public createLocalCard(stream: MediaStream, username: string): HTMLElement {
    if (this.localCard && this.localCard.parentNode) {
      this.localCard.parentNode.removeChild(this.localCard);
    }

    const card = this.buildCardElement('local', `${username} (You)`, true);
    const video = card.querySelector('video') as HTMLVideoElement;
    if (video) {
      video.srcObject = stream;
      video.muted = true;
    }

    this.localCard = card;
    this.cards.set('local', card);
    this.renderLayout();

    return card;
  }

  public createPeerCard(peerId: string, username: string, initialState: Partial<PeerInfo> = {}): HTMLElement {
    const existingCard = this.cards.get(peerId);
    if (existingCard) {
      this.updatePeerState(peerId, initialState);
      return existingCard;
    }

    const card = this.buildCardElement(peerId, username, false);
    this.cards.set(peerId, card);
    this.updatePeerState(peerId, initialState);
    this.renderLayout();

    if (this.isScreenId(peerId)) {
      this.setSpotlight(peerId);
    }

    return card;
  }

  private buildCardElement(id: string, username: string, isLocal: boolean): HTMLElement {
    const card = document.createElement('div');
    card.className = `peer-card is-landscape ${isLocal ? 'local-card' : ''}`;
    card.setAttribute('data-peer-id', id);
    card.setAttribute('role', 'button');
    card.setAttribute('tabindex', '0');
    card.setAttribute('aria-label', `Focus ${username}`);

    const initials = this.getInitials(username);

    card.innerHTML = `
      <video autoplay playsinline ${isLocal ? 'muted' : ''}></video>
      <div class="peer-placeholder">
        <div class="peer-avatar"></div>
      </div>
      <div class="audio-ring"></div>
      <div class="peer-overlay">
        <span class="peer-name"></span>
        <div class="peer-status">
          <span class="stream-muted-badge" title="Muted for you" aria-label="Muted for you">
            <i class="fa-solid fa-volume-xmark" aria-hidden="true"></i>
          </span>
          <span class="mic-badge" title="Microphone muted" aria-label="Microphone muted">
            <i class="fa-solid fa-microphone-slash" aria-hidden="true"></i>
          </span>
          <span class="connection-dot" data-state="new" aria-label="Connecting"></span>
        </div>
      </div>
      <button type="button" class="pip-move-handle" aria-label="Move camera" title="Move camera">
        <i class="fa-solid fa-up-down-left-right" aria-hidden="true"></i>
      </button>
      <button type="button" class="pip-resize-handle" aria-label="Resize camera" title="Resize camera"></button>
    `;

    // textContent everywhere — usernames are untrusted (XSS-safe)
    const avatar = card.querySelector('.peer-avatar') as HTMLElement;
    if (avatar) avatar.textContent = initials;
    const nameEl = card.querySelector('.peer-name') as HTMLElement;
    if (nameEl) nameEl.textContent = username;

    const isRemoteStream = id !== 'local' && !id.startsWith('local-');
    // Any remote tile can carry audio — a screen share may include
    // tab/system audio, so mute + volume are available from the same
    // popover on screen-share tiles too (right-click / ContextMenu key).
    const canControlAudio = isRemoteStream;
    // While a screen share is pinned, the other cameras become draggable PiP
    // bubbles and are no longer valid "pin" targets themselves — clicking one
    // shouldn't accidentally un-pin the screen everyone is watching.
    const canTogglePin = () => !(this.isPipMode() && id !== this.spotlightId);
    const toggleSpotlight = () => {
      if (!canTogglePin()) return;
      this.setSpotlight(this.spotlightId === id ? null : id);
    };
    card.addEventListener('click', (event) => {
      if ((event.target as HTMLElement).closest('.pip-move-handle, .pip-resize-handle')) return;
      toggleSpotlight();
    });
    card.addEventListener('keydown', (event) => {
      if ((event.target as HTMLElement).closest('.pip-move-handle, .pip-resize-handle')) return;
      if (canControlAudio && (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10'))) {
        event.preventDefault();
        const rect = card.getBoundingClientRect();
        this.openAudioPopover(id, this.getCardUsername(card), rect.left + 16, rect.bottom - 16, card);
        return;
      }
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        toggleSpotlight();
      }
    });

    this.wirePipHandles(card, id);
    const video = card.querySelector('video') as HTMLVideoElement;
    if (canControlAudio) {
      video.addEventListener('contextmenu', (event) => {
        event.preventDefault();
        this.openAudioPopover(id, this.getCardUsername(card), event.clientX, event.clientY, card);
      });
    }
    video.addEventListener('loadedmetadata', () => this.updateVideoOrientation(card, video));

    return card;
  }

  private updateVideoOrientation(card: HTMLElement, video: HTMLVideoElement): void {
    if (video.videoWidth === 0 || video.videoHeight === 0) return;
    const isPortrait = video.videoHeight > video.videoWidth;
    card.classList.toggle('is-portrait', isPortrait);
    card.classList.toggle('is-landscape', !isPortrait);
    this.updateGalleryLayout();
  }

  private getAudioState(peerId: string): PeerAudioState {
    let state = this.peerAudio.get(peerId);
    if (!state) {
      state = { muted: false, volume: 1 };
      this.peerAudio.set(peerId, state);
    }
    return state;
  }

  /**
   * Mute/unmute ONE peer's stream (Discord-style). Local-only preference;
   * deafen still overrides everything while active.
   */
  public setPeerMuted(peerId: string, muted: boolean): void {
    this.getAudioState(peerId).muted = muted;
    const card = this.cards.get(peerId);
    if (!card) return;

    card.classList.toggle('stream-muted', muted);
    const video = card.querySelector('video') as HTMLVideoElement;
    if (video) {
      video.muted = this.deafened || muted;
    }
    this.updateAudioPopover(peerId);
  }

  /**
   * Set ONE peer's stream volume, 0..100% (Discord-style; HTMLMediaElement
   * volume is spec-limited to 0..1 — a >100% boost would need a Web Audio
   * gain node, not this API).
   */
  public setPeerVolume(peerId: string, volume: number): void {
    const clamped = Math.max(0, Math.min(1, volume));
    this.getAudioState(peerId).volume = clamped;
    const card = this.cards.get(peerId);
    if (!card) return;

    const video = card.querySelector('video') as HTMLVideoElement;
    if (video) video.volume = clamped;
    this.updateAudioPopover(peerId);
  }

  private applyAudioState(peerId: string, video: HTMLVideoElement): void {
    const state = this.getAudioState(peerId);
    video.muted = this.deafened || state.muted;
    video.volume = state.volume;
    const card = this.cards.get(peerId);
    card?.classList.toggle('stream-muted', state.muted);
  }

  public removePeerCard(peerId: string): void {
    const card = this.cards.get(peerId);
    if (card) {
      const restoreFocus = card.contains(document.activeElement);
      if (card.parentNode) {
        card.parentNode.removeChild(card);
      }
      this.cards.delete(peerId);
      this.peerAudio.delete(peerId);
      this.pipPositions.delete(peerId);
      if (this.audioPopoverPeerId === peerId) this.closeAudioPopover();
      if (this.spotlightId === peerId) {
        this.setSpotlight(null);
      }
      this.renderLayout();
      if (restoreFocus) {
        const nextCard = this.cards.values().next().value as HTMLElement | undefined;
        nextCard?.focus();
      }
    }
  }

  public setPeerStream(peerId: string, stream: MediaStream): void {
    const card = this.cards.get(peerId);
    if (card) {
      const video = card.querySelector('video') as HTMLVideoElement;
      if (video) {
        // Live MediaStreams propagate track additions — only re-assign when the
        // stream object itself changes, so an arriving video track doesn't
        // interrupt an in-flight play() (AbortError race).
        if (video.srcObject !== stream) {
          video.srcObject = stream;
          // Local previews (webcam + screen) are always muted so shared
          // screen audio doesn't echo back into the mic.
          const isPreview = peerId === 'local' || peerId === 'local-screen';
          if (isPreview) {
            video.muted = true;
          } else {
            // Per-peer audio prefs (mute + volume) apply to every attach,
            // including reconnections (Discord-style).
            this.applyAudioState(peerId, video);
          }
          const play = () => {
            if (typeof video.play !== 'function') return;
            video
              .play()
              .then(() => {
                // Decisive diagnostic: if a screen card logs this WITH an
                // audio track and there's still no sound, the captured
                // track itself is silent (source-side problem).
                console.log(
                  `[UI] ${peerId} playing — muted=${video.muted}, volume=${video.volume}, audioTracks=${stream.getAudioTracks().length}`
                );
              })
              .catch((e) => {
                // AbortError = interrupted by a newer load — expected, not a bug.
                if ((e as DOMException | undefined)?.name !== 'AbortError') {
                  console.warn(`[UI] Video play error (${peerId}):`, e);
                }
              });
          };
          play();
          this.updateVideoOrientation(card, video);
          // If the stream had no audio yet (screen video often arrives before
          // its audio track), retry playback once audio lands — some browsers
          // won't restart a video element that started audio-less.
          if (
            !isPreview &&
            typeof stream.addEventListener === 'function' &&
            stream.getAudioTracks().length === 0
          ) {
            const onAdd = () => {
              play();
              stream.removeEventListener('addtrack', onAdd);
            };
            stream.addEventListener('addtrack', onAdd);
          }
        }
      }
    }
  }

  /**
   * Toggle deafen state. Mutes every remote stream while active; on release,
   * per-peer mutes (Discord-style) are restored.
   */
  public setDeafened(deafened: boolean): void {
    this.deafened = deafened;
    this.cards.forEach((card, id) => {
      if (id === 'local') return;
      const video = card.querySelector('video') as HTMLVideoElement;
      if (video) {
        video.muted = deafened || this.getAudioState(id).muted;
      }
    });
  }

  public updatePeerState(peerId: string, state: Partial<PeerInfo>): void {
    const card = this.cards.get(peerId);
    if (!card) return;

    if (state.username !== undefined) {
      const nameEl = card.querySelector('.peer-name') as HTMLElement;
      if (nameEl) nameEl.textContent = state.username;
      const focused = card.classList.contains('spotlight');
      card.setAttribute('aria-label', focused ? `Unpin ${state.username}` : `Focus ${state.username}`);
      const avatar = card.querySelector('.peer-avatar') as HTMLElement;
      if (avatar) avatar.textContent = this.getInitials(state.username);
    }

    if (state.isMuted !== undefined) {
      const badge = card.querySelector('.mic-badge');
      if (badge) {
        badge.classList.toggle('muted', state.isMuted);
        badge.setAttribute('title', state.isMuted ? 'Microphone muted' : 'Microphone on');
        badge.setAttribute('aria-label', state.isMuted ? 'Microphone muted' : 'Microphone on');
      }
    }

    if (state.isCamOff !== undefined) {
      const video = card.querySelector('video');
      const placeholder = card.querySelector('.peer-placeholder') as HTMLElement;
      if (state.isCamOff) {
        card.classList.add('cam-off');
        if (video) video.style.display = 'none';
        if (placeholder) placeholder.style.display = 'flex';
      } else {
        card.classList.remove('cam-off');
        if (video) video.style.display = 'block';
        if (placeholder) placeholder.style.display = 'none';
      }
    }
  }

  public setAudioLevel(target: string, level: number): void {
    const cardId = target === 'local' ? 'local' : target;
    const card = this.cards.get(cardId);
    if (card) {
      card.style.setProperty('--audio-level', `${level}%`);
      if (level > 15) {
        card.classList.add('speaking');
      } else {
        card.classList.remove('speaking');
      }
    }
  }

  public setConnectionQuality(peerId: string, state: RTCIceConnectionState): void {
    const card = this.cards.get(peerId);
    if (!card) return;
    const dot = card.querySelector('.connection-dot');
    if (!dot) return;

    const quality =
      state === 'connected'
        ? 'connected'
        : state === 'failed' || state === 'disconnected'
          ? 'failed'
          : 'connecting';

    dot.setAttribute('data-state', quality);
    dot.classList.toggle('connected', quality === 'connected');
    dot.classList.toggle('connecting', quality === 'connecting');
    dot.classList.toggle('failed', quality === 'failed');
  }

  public setSpotlight(peerId: string | null): void {
    const prev = this.spotlightId;
    this.spotlightId = peerId;
    // Only clear PiP positions when entering a non-screen card or switching
    // to a different screen share.  Exiting to null (gallery) keeps positions
    // so dragged cameras persist when re-entering the same share.
    if (peerId && (!this.isScreenId(peerId) || (prev !== null && peerId !== prev))) {
      this.pipPositions.clear();
    }
    this.renderLayout();
  }

  private renderLayout(): void {
    const selfSharePinned = this.isSelfScreenShare(this.spotlightId);
    const visibleCount = this.cards.size - (this.cards.has('local-screen') ? 1 : 0);
    this.container.setAttribute('data-peer-count', visibleCount.toString());

    if (this.spotlightId && this.cards.has(this.spotlightId) && !selfSharePinned) {
      this.container.classList.add('has-spotlight');
      const pip = this.isPipMode();
      this.cameraStrip.classList.toggle('pip-mode', pip);

      if (!this.cameraStrip.parentNode) {
        this.container.appendChild(this.cameraStrip);
      }

      this.cards.forEach((card, id) => {
        if (id === this.spotlightId) {
          card.classList.add('spotlight');
          card.setAttribute('role', 'button');
          card.setAttribute('tabindex', '0');
          card.setAttribute('aria-pressed', 'true');
          card.setAttribute('aria-label', `Unpin ${this.getCardUsername(card)}`);
          this.clearPipGeometry(card);
          if (card.parentNode !== this.container) {
            this.container.appendChild(card);
          }
        } else if (this.isSelfScreenShare(id)) {
          // The sharer never sees their own screen share — not fullscreen,
          // not as a bubble. Peers still receive it; this is a local-only
          // view decision.
          card.classList.remove('spotlight');
          this.clearPipGeometry(card);
          if (card.parentNode) card.parentNode.removeChild(card);
        } else {
          card.classList.remove('spotlight');
          if (pip) {
            // Movable/resizable camera bubble floating over the shared screen.
            card.setAttribute('role', 'group');
            card.setAttribute('tabindex', '-1');
            card.removeAttribute('aria-pressed');
            card.setAttribute('aria-label', `${this.getCardUsername(card)} camera — movable`);
          } else {
            // Simple, fixed Discord-style thumbnail.
            card.setAttribute('role', 'button');
            card.setAttribute('tabindex', '0');
            card.setAttribute('aria-pressed', 'false');
            card.setAttribute('aria-label', `Focus ${this.getCardUsername(card)}`);
            this.clearPipGeometry(card);
          }
          if (card.parentNode !== this.cameraStrip) {
            this.cameraStrip.appendChild(card);
          }
        }
      });

      if (pip) this.layoutPipTiles();
    } else {
      this.container.classList.remove('has-spotlight');
      this.cameraStrip.classList.remove('pip-mode');

      if (this.cameraStrip.parentNode) {
        this.cameraStrip.parentNode.removeChild(this.cameraStrip);
      }

      this.cards.forEach((card, id) => {
        card.classList.remove('spotlight');
        if (this.isSelfScreenShare(id)) {
          // Own share preview: detached, never rendered to the sharer.
          this.clearPipGeometry(card);
          if (card.parentNode) card.parentNode.removeChild(card);
          return;
        }
        card.setAttribute('role', 'button');
        card.setAttribute('tabindex', '0');
        card.setAttribute('aria-pressed', 'false');
        card.setAttribute('aria-label', `Focus ${this.getCardUsername(card)}`);
        this.clearPipGeometry(card);
        if (card.parentNode !== this.container) {
          this.container.appendChild(card);
        }
      });
    }

    this.updateGalleryLayout();
  }
}
