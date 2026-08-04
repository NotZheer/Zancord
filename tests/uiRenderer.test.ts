// @vitest-environment happy-dom
import { describe, it, expect, beforeEach } from 'vitest';
import { UIRenderer } from '../src/ui/UIRenderer';
import { EventBus } from '../src/core/EventBus';

// happy-dom does not implement srcObject on media elements.
Object.defineProperty(window.HTMLMediaElement.prototype, 'srcObject', {
  configurable: true,
  writable: true,
  value: null,
});

function makeRenderer(width = 1200, height = 700): { renderer: UIRenderer; container: HTMLElement } {
  document.body.innerHTML = '';
  const container = document.createElement('div');
  container.id = 'call-grid';
  Object.defineProperty(container, 'getBoundingClientRect', {
    configurable: true,
    value: () => ({
      width,
      height,
      top: 0,
      right: width,
      bottom: height,
      left: 0,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    }),
  });
  document.body.appendChild(container);
  const renderer = new UIRenderer(container, new EventBus());
  return { renderer, container };
}

const fakeStream = () => ({ id: 's1' }) as unknown as MediaStream;

describe('peer cards (U3, U4)', () => {
  it('renders a name label on peer cards', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    const name = document.querySelector('[data-peer-id="p1"] .peer-name');
    expect(name?.textContent).toBe('Alice');
  });

  it('renders names safely (no HTML injection)', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', '<img src=x onerror="alert(1)">');
    const card = document.querySelector('[data-peer-id="p1"]');
    expect(card?.querySelector('img')).toBeNull();
    expect(card?.querySelector('.peer-name')?.textContent).toContain('<img');
  });

  it('shows the mic-mute badge when a peer mutes and hides it when unmuted', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.updatePeerState('p1', { isMuted: true });
    const badge = document.querySelector('[data-peer-id="p1"] .mic-badge');
    expect(badge?.classList.contains('muted')).toBe(true);
    renderer.updatePeerState('p1', { isMuted: false });
    expect(badge?.classList.contains('muted')).toBe(false);
  });

  it('updates the name label when a peer changes their username', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.updatePeerState('p1', { username: 'Alice B' });
    expect(document.querySelector('[data-peer-id="p1"] .peer-name')?.textContent).toBe('Alice B');
  });

  it('shows the placeholder when a peer turns their camera off', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    const placeholder = document.querySelector('[data-peer-id="p1"] .peer-placeholder') as HTMLElement;
    placeholder.style.display = 'none';
    renderer.updatePeerState('p1', { isCamOff: true });
    expect(placeholder.style.display).toBe('flex');
  });

  it('applies the speaking glow from audio level', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.setAudioLevel('p1', 60);
    expect(document.querySelector('[data-peer-id="p1"]')?.classList.contains('speaking')).toBe(true);
    renderer.setAudioLevel('p1', 5);
    expect(document.querySelector('[data-peer-id="p1"]')?.classList.contains('speaking')).toBe(false);
  });
});

describe('clean participant tiles', () => {
  it('does not render per-stream control clutter or a false muted state', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');

    const card = document.querySelector('[data-peer-id="p1"]');
    expect(card?.querySelector('.peer-controls')).toBeNull();
    // The muted-for-you badge exists but stays inert (no .stream-muted
    // class) until a peer's stream is actually muted by the user.
    expect(card?.querySelector('.stream-muted-badge')).not.toBeNull();
    expect(card?.classList.contains('stream-muted')).toBe(false);
    expect(card?.querySelector('.mic-badge')?.classList.contains('muted')).toBe(false);
  });

  it('shows a muted state immediately when a peer joins muted', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice', { isMuted: true });

    expect(document.querySelector('[data-peer-id="p1"] .mic-badge')?.classList.contains('muted')).toBe(true);
  });

  it('refreshes state when an existing peer is received again', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p1', 'Alice', { isMuted: true, isCamOff: true });

    const card = document.querySelector('[data-peer-id="p1"]');
    expect(card?.querySelector('.mic-badge')?.classList.contains('muted')).toBe(true);
    expect(card?.classList.contains('cam-off')).toBe(true);
  });

  it('uses the stream’s intrinsic dimensions to identify portrait cameras', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');

    const card = document.querySelector('[data-peer-id="p1"]') as HTMLElement;
    const video = card.querySelector('video') as HTMLVideoElement;
    Object.defineProperties(video, {
      videoWidth: { configurable: true, value: 720 },
      videoHeight: { configurable: true, value: 1280 },
    });
    video.dispatchEvent(new Event('loadedmetadata'));

    expect(card.classList.contains('is-portrait')).toBe(true);
    expect(card.classList.contains('is-landscape')).toBe(false);
  });



  it('calculates side-by-side 16:9 cells for two cameras in a wide window', () => {
    const { renderer, container } = makeRenderer(1200, 700);
    renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p2', 'Bob');

    expect(container.style.getPropertyValue('--gallery-columns')).toBe('2');
    expect(container.style.getPropertyValue('--gallery-cell-width')).toBe('592px');
    expect(container.style.getPropertyValue('--gallery-cell-height')).toBe('333px');
  });

  it('keeps 16:9 geometry at odd container widths', () => {
    const { renderer, container } = makeRenderer(1202, 700);
    renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p2', 'Bob');

    expect(container.style.getPropertyValue('--gallery-cell-width')).toBe('593px');
    expect(container.style.getPropertyValue('--gallery-cell-height')).toBe('333.5625px');
  });

  it('opens a custom remote-audio popover from a stream right-click', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.setPeerStream('p1', fakeStream());
    const card = document.querySelector('[data-peer-id="p1"]') as HTMLElement;
    const video = card.querySelector('video') as HTMLVideoElement;

    video.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 80, clientY: 96 }));

    const popover = document.querySelector('.stream-audio-popover') as HTMLElement;
    const muteButton = popover.querySelector('.stream-audio-mute') as HTMLButtonElement;
    const volume = popover.querySelector('.stream-audio-volume') as HTMLInputElement;
    expect(popover.hidden).toBe(false);
    expect(popover.textContent).toContain('Alice');

    muteButton.click();
    expect(video.muted).toBe(true);
    expect(muteButton.textContent).toContain('Unmute');

    volume.value = '45';
    volume.dispatchEvent(new Event('input', { bubbles: true }));
    expect(video.volume).toBe(0.45);
  });

  it('does not replace the browser context menu when card chrome is right-clicked', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    const name = document.querySelector('[data-peer-id="p1"] .peer-name') as HTMLElement;

    name.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 80, clientY: 96 }));

    expect((document.querySelector('.stream-audio-popover') as HTMLElement).hidden).toBe(true);
  });

  it('uses a current participant name when opening stream audio controls', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.updatePeerState('p1', { username: 'Alice B' });
    const video = document.querySelector('[data-peer-id="p1"] video') as HTMLVideoElement;

    video.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 80, clientY: 96 }));

    expect(document.querySelector('.stream-audio-popover')?.textContent).toContain('Alice B');
  });

  it('announces which participant is focused and restores focus when one leaves', () => {
    const { renderer } = makeRenderer();
    const first = renderer.createPeerCard('p1', 'Alice');
    const second = renderer.createPeerCard('p2', 'Bob');

    renderer.setSpotlight('p1');
    expect(first.getAttribute('role')).toBe('button');
    expect(first.getAttribute('aria-pressed')).toBe('true');
    expect(second.getAttribute('role')).toBe('button');
    expect(second.getAttribute('aria-pressed')).toBe('false');

    first.focus();
    renderer.removePeerCard('p1');
    expect(document.activeElement).toBe(second);
    expect(second.getAttribute('aria-pressed')).toBe('false');
  });
});

describe('screen-share camera bubbles (PiP)', () => {
  it('keeps a pinned camera simple: no drag/resize, fixed strip thumbnail', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    const bob = renderer.createPeerCard('p2', 'Bob');
    renderer.setSpotlight('p1');

    const strip = document.getElementById('camera-strip') as HTMLElement;
    expect(strip.classList.contains('pip-mode')).toBe(false);
    expect(bob.getAttribute('role')).toBe('button');
    expect(bob.getAttribute('tabindex')).toBe('0');
    expect(bob.style.getPropertyValue('--pip-left')).toBe('');
  });

  it('turns other cameras into movable, resizable bubbles while a screen share is pinned', () => {
    const { renderer } = makeRenderer();
    const alice = renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p1-screen', "Alice's Screen");

    const strip = document.getElementById('camera-strip') as HTMLElement;
    expect(strip.classList.contains('pip-mode')).toBe(true);
    expect(alice.getAttribute('role')).toBe('group');
    expect(alice.getAttribute('tabindex')).toBe('-1');
    expect(alice.querySelector('.pip-move-handle')).not.toBeNull();
    expect(alice.querySelector('.pip-resize-handle')).not.toBeNull();
    expect(alice.style.getPropertyValue('--pip-left')).not.toBe('');
    expect(alice.style.getPropertyValue('--pip-width')).not.toBe('');
  });

  it('does not let clicking a camera bubble unpin an active screen share', () => {
    const { renderer } = makeRenderer();
    const alice = renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p1-screen', "Alice's Screen");

    alice.click();

    expect(document.querySelector('[data-peer-id="p1-screen"]')?.classList.contains('spotlight')).toBe(true);
  });



  it('drags a camera bubble freely by grabbing the card itself', () => {
    const { renderer } = makeRenderer();
    const alice = renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p1-screen', "Alice's Screen");
    (alice as any).setPointerCapture = () => {};
    (alice as any).releasePointerCapture = () => {};

    const beforeLeft = alice.style.getPropertyValue('--pip-left');
    alice.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, pointerId: 1, clientX: 100, clientY: 100 }));
    alice.dispatchEvent(new PointerEvent('pointermove', { bubbles: true, pointerId: 1, clientX: 320, clientY: 260 }));

    expect(alice.style.getPropertyValue('--pip-left')).not.toBe(beforeLeft);
  });

  it('keeps the local user’s own camera as a bubble during a remote screen share', () => {
    const { renderer } = makeRenderer();
    const local = renderer.createLocalCard(fakeStream(), 'Me');
    renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p1-screen', "Alice's Screen");

    const strip = document.getElementById('camera-strip') as HTMLElement;
    expect(strip.classList.contains('pip-mode')).toBe(true);
    expect(local.parentNode).toBe(strip);
  });

  it('does not show the local user their own screen share', () => {
    const { renderer, container } = makeRenderer(1200, 700);
    const local = renderer.createLocalCard(fakeStream(), 'Me');
    renderer.createPeerCard('p1', 'Alice');
    const screen = renderer.createPeerCard('local-screen', "My Screen");

    // The sharer keeps the normal gallery: no spotlight, own screen tile
    // never rendered anywhere in their own view, own camera still visible.
    expect(container.classList.contains('has-spotlight')).toBe(false);
    expect(screen.parentNode).toBeNull();
    expect(local.parentNode).toBe(container);
    expect(document.querySelector('[data-peer-id="p1"]')?.parentNode).toBe(container);
    expect(container.style.getPropertyValue('--gallery-columns')).toBe('2');

    renderer.removePeerCard('local-screen');
    expect(screen.parentNode).toBeNull();
  });


  it('clears camera bubble positions once the screen share ends', () => {
    const { renderer } = makeRenderer();
    const alice = renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p1-screen', "Alice's Screen");
    expect(alice.style.getPropertyValue('--pip-left')).not.toBe('');

    renderer.removePeerCard('p1-screen');

    expect(alice.style.getPropertyValue('--pip-left')).toBe('');
    expect(alice.getAttribute('role')).toBe('button');
    expect(alice.getAttribute('tabindex')).toBe('0');
  });

  it('opens the audio popover from a remote screen-share tile (mute/volume for share audio)', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p1-screen', "Alice's Screen");
    const screenVideo = document.querySelector('[data-peer-id="p1-screen"] video') as HTMLVideoElement;

    screenVideo.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 80, clientY: 96 }));

    const popover = document.querySelector('.stream-audio-popover') as HTMLElement;
    expect(popover.hidden).toBe(false);
    expect(popover.textContent).toContain("Alice's Screen");

    const muteButton = popover.querySelector('.stream-audio-mute') as HTMLButtonElement;
    muteButton.click();
    expect(screenVideo.muted).toBe(true);
  });
});

describe('muted-for-you indicator', () => {
  it('shows a persistent muted-for-you badge on the tile after the popover closes', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.setPeerMuted('p1', true);

    const card = document.querySelector('[data-peer-id="p1"]');
    expect(card?.querySelector('.stream-muted-badge')).not.toBeNull();
    expect(card?.classList.contains('stream-muted')).toBe(true);

    renderer.setPeerMuted('p1', false);
    expect(card?.classList.contains('stream-muted')).toBe(false);
  });
});

describe('stream attachment (play() race)', () => {
  it('assigns srcObject once per stream and does not restart playback for the same stream', () => {
    let playCount = 0;
    const originalPlay = window.HTMLMediaElement.prototype.play;
    window.HTMLMediaElement.prototype.play = function () {
      playCount++;
      return Promise.resolve();
    } as any;
    try {
      const { renderer } = makeRenderer();
      renderer.createPeerCard('p1', 'Alice');
      const stream = fakeStream();
      renderer.setPeerStream('p1', stream);
      const video = document.querySelector('[data-peer-id="p1"] video') as HTMLVideoElement;
      expect(video.srcObject).toBe(stream);
      expect(playCount).toBe(1);

      // Same stream object (a live MediaStream that gained a track) → no reassignment.
      renderer.setPeerStream('p1', stream);
      expect(playCount).toBe(1);

      // A genuinely new stream → reassigned and played.
      const stream2 = { id: 's2' } as unknown as MediaStream;
      renderer.setPeerStream('p1', stream2);
      expect(video.srcObject).toBe(stream2);
      expect(playCount).toBe(2);
    } finally {
      window.HTMLMediaElement.prototype.play = originalPlay;
    }
  });

  it('retries playback when a remote stream gains its audio track later (screen share)', () => {
    let playCount = 0;
    const originalPlay = window.HTMLMediaElement.prototype.play;
    window.HTMLMediaElement.prototype.play = function () {
      playCount++;
      return Promise.resolve();
    } as any;
    try {
      const { renderer } = makeRenderer();
      renderer.createPeerCard('p1', 'Alice');
      const listeners: Record<string, (() => void)[]> = {};
      const stream = {
        id: 's-screen',
        getAudioTracks: () => [],
        addEventListener: (name: string, cb: () => void) => {
          (listeners[name] ||= []).push(cb);
        },
        removeEventListener: (name: string, cb: () => void) => {
          listeners[name] = (listeners[name] || []).filter((c) => c !== cb);
        },
      } as unknown as MediaStream;
      renderer.setPeerStream('p1', stream);
      expect(playCount).toBe(1);

      // Audio track lands → playback retried.
      listeners['addtrack']?.forEach((cb) => cb());
      expect(playCount).toBe(2);
    } finally {
      window.HTMLMediaElement.prototype.play = originalPlay;
    }
  });
});

describe('deafen (U2)', () => {
  it('mutes existing remote videos when deafened', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.setPeerStream('p1', fakeStream());
    renderer.setDeafened(true);
    const video = document.querySelector('[data-peer-id="p1"] video') as HTMLVideoElement;
    expect(video.muted).toBe(true);
    renderer.setDeafened(false);
    expect(video.muted).toBe(false);
  });

  it('mutes peers who join after deafen was enabled', () => {
    const { renderer } = makeRenderer();
    renderer.setDeafened(true);
    renderer.createPeerCard('p1', 'Alice');
    renderer.setPeerStream('p1', fakeStream());
    const video = document.querySelector('[data-peer-id="p1"] video') as HTMLVideoElement;
    expect(video.muted).toBe(true);
  });

  it('mutes the local screen preview so shared audio does not echo', () => {
    const { renderer } = makeRenderer();
    // The self-share tile is detached from the DOM in the sharer's own view
    // (they never see their own screen), so grab it from the returned card.
    const card = renderer.createPeerCard('local-screen', "My Screen");
    renderer.setPeerStream('local-screen', fakeStream());
    const video = card.querySelector('video') as HTMLVideoElement;
    expect(video.muted).toBe(true);
  });
});

describe('per-peer audio controls (Discord-style)', () => {
  it('mutes and unmutes a single peer stream', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.setPeerStream('p1', fakeStream());
    const video = document.querySelector('[data-peer-id="p1"] video') as HTMLVideoElement;
    renderer.setPeerMuted('p1', true);
    expect(video.muted).toBe(true);
    renderer.setPeerMuted('p1', false);
    expect(video.muted).toBe(false);
  });

  it('applies a saved mute to a stream attached later (reconnection)', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.setPeerMuted('p1', true);
    renderer.setPeerStream('p1', fakeStream());
    const video = document.querySelector('[data-peer-id="p1"] video') as HTMLVideoElement;
    expect(video.muted).toBe(true);
  });

  it('sets per-peer volume and clamps it to 0..100% (spec limit)', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.setPeerStream('p1', fakeStream());
    const video = document.querySelector('[data-peer-id="p1"] video') as HTMLVideoElement;
    renderer.setPeerVolume('p1', 0.5);
    expect(video.volume).toBe(0.5);
    renderer.setPeerVolume('p1', 3);
    expect(video.volume).toBe(1);
    renderer.setPeerVolume('p1', -1);
    expect(video.volume).toBe(0);
  });

  it('undeafen restores per-peer mutes', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.createPeerCard('p2', 'Bob');
    renderer.setPeerStream('p1', fakeStream());
    renderer.setPeerStream('p2', fakeStream());
    renderer.setPeerMuted('p1', true);
    renderer.setDeafened(true);
    const v1 = document.querySelector('[data-peer-id="p1"] video') as HTMLVideoElement;
    const v2 = document.querySelector('[data-peer-id="p2"] video') as HTMLVideoElement;
    expect(v1.muted).toBe(true);
    expect(v2.muted).toBe(true);
    renderer.setDeafened(false);
    expect(v1.muted).toBe(true); // p1 stays muted by the user
    expect(v2.muted).toBe(false); // p2 restored
  });

  it('reflects the stream-muted state on the card', () => {
    const { renderer } = makeRenderer();
    renderer.createPeerCard('p1', 'Alice');
    renderer.setPeerMuted('p1', true);
    const card = document.querySelector('[data-peer-id="p1"]');
    expect(card?.classList.contains('stream-muted')).toBe(true);
  });
});
