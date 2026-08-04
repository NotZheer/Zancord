import { describe, it, expect, beforeEach } from 'vitest';
import { EventBus } from '../src/core/EventBus';
import { PeerManager } from '../src/core/PeerManager';
import { Events } from '../src/types';
import {
  FakeRTCPeerConnection,
  FakeMediaStream,
  FakeMediaStreamTrack,
  installFakeWebRTC,
  flushMicrotasks,
} from './helpers/fakeRtc';

type Signal = { targetId: string; signal: { sdp?: { type: string; sdp: string }; candidate?: unknown } };

function makeManager(id: string, withMedia = true): { pm: PeerManager; bus: EventBus; signals: Signal[] } {
  const bus = new EventBus();
  const pm = new PeerManager(bus);
  pm.setLocalId(id);
  if (withMedia) {
    pm.setLocalStream(
      new FakeMediaStream([new FakeMediaStreamTrack('audio'), new FakeMediaStreamTrack('video')])
    );
  }
  const signals: Signal[] = [];
  bus.on('rtc-send-signal', (s: Signal) => signals.push(s));
  return { pm, bus, signals };
}

beforeEach(() => {
  installFakeWebRTC();
});

describe('screen share negotiation (C3)', () => {
  it('emits an offer when ANY peer adds a screen track on a stable connection', async () => {
    const { pm, bus, signals } = makeManager('A');
    // Connect to peer B without initiator privileges.
    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    await flushMicrotasks();

    // Settle the initial negotiation (offer out, answer back → stable),
    // like a real browser does before the next negotiationneeded fires.
    const pc = FakeRTCPeerConnection.instances[0];
    await pc.setRemoteDescription({ type: 'answer', sdp: 'answer-1' });
    const offersBefore = signals.filter((s) => s.signal.sdp?.type === 'offer').length;

    pm.addScreenTrack(new FakeMediaStream([new FakeMediaStreamTrack('video')]));
    await flushMicrotasks();

    const offersAfter = signals.filter((s) => s.signal.sdp?.type === 'offer').length;
    expect(offersAfter).toBe(offersBefore + 1);
    expect(signals[signals.length - 1].targetId).toBe('B');
  });

  it('converges when both peers offer simultaneously (glare handling)', async () => {
    const a = makeManager('A');
    const b = makeManager('B');
    // Wire the two managers' signaling together end-to-end.
    a.bus.on('rtc-send-signal', ({ targetId, signal }) => {
      if (targetId === 'B') b.bus.emit('rtc-signal-received', { senderId: 'A', signal });
    });
    b.bus.on('rtc-send-signal', ({ targetId, signal }) => {
      if (targetId === 'A') a.bus.emit('rtc-signal-received', { senderId: 'B', signal });
    });

    a.bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    b.bus.emit(Events.PEER_JOINED, { id: 'A', username: 'Ana' });
    await flushMicrotasks();

    // Both start screen sharing at the same moment.
    a.pm.addScreenTrack(new FakeMediaStream([new FakeMediaStreamTrack('video')]));
    b.pm.addScreenTrack(new FakeMediaStream([new FakeMediaStreamTrack('video')]));
    await flushMicrotasks();
    await flushMicrotasks();
    await flushMicrotasks();

    const pcA = FakeRTCPeerConnection.instances[0];
    const pcB = FakeRTCPeerConnection.instances[1];
    expect(pcA.remoteDescription?.type).toBeDefined();
    expect(pcB.remoteDescription?.type).toBeDefined();
    expect(pcA.signalingState).toBe('stable');
    expect(pcB.signalingState).toBe('stable');
  });
});

describe('WebRTC config (P7: mesh transport efficiency)', () => {
  it('forces a single bundled transport per peer (max-bundle)', () => {
    const { pm, bus } = makeManager('A');
    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    const pc = FakeRTCPeerConnection.instances[0];
    expect(pc.config.bundlePolicy).toBe('max-bundle');
  });

  it('never configures STUN/TURN servers (Tailscale-only rule)', () => {
    const { pm, bus } = makeManager('A');
    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    const pc = FakeRTCPeerConnection.instances[0];
    expect(pc.config.iceServers).toEqual([]);
  });
});

describe('offer/answer + ICE (lock-in)', () => {
  it('answers a remote offer', async () => {
    const { bus, signals } = makeManager('A');
    const offer: Signal['signal'] = { sdp: { type: 'offer', sdp: 'offer-1' } };

    bus.emit('rtc-signal-received', { senderId: 'B', signal: offer });
    await flushMicrotasks();

    const pc = FakeRTCPeerConnection.instances[0];
    expect(pc.remoteDescription?.type).toBe('offer');
    const answer = signals.find((s) => s.signal.sdp?.type === 'answer');
    expect(answer).toBeTruthy();
    expect(answer?.targetId).toBe('B');
  });

  it('queues ICE candidates until the remote description is set, then flushes', async () => {
    const { bus, signals } = makeManager('A');
    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    await flushMicrotasks();
    // A's offer is out; B's answer hasn't arrived yet.
    expect(signals.some((s) => s.signal.sdp?.type === 'offer')).toBe(true);

    // Candidate arrives before the remote description → queued.
    bus.emit('rtc-signal-received', {
      senderId: 'B',
      signal: { candidate: { candidate: 'early-candidate' } },
    });
    await flushMicrotasks();

    const pc = FakeRTCPeerConnection.instances[0];
    expect(pc.receivedCandidates).not.toEqual(
      expect.arrayContaining([expect.objectContaining({ candidate: 'early-candidate' })])
    );

    // B's answer arrives → queued candidates are flushed.
    bus.emit('rtc-signal-received', {
      senderId: 'B',
      signal: { sdp: { type: 'answer', sdp: 'answer-1' } },
    });
    await flushMicrotasks();

    expect(pc.remoteDescription?.type).toBe('answer');
    expect(pc.receivedCandidates).toEqual(
      expect.arrayContaining([expect.objectContaining({ candidate: 'early-candidate' })])
    );

    // Late candidate after SDP → added immediately.
    bus.emit('rtc-signal-received', {
      senderId: 'B',
      signal: { candidate: { candidate: 'late-candidate' } },
    });
    await flushMicrotasks();
    expect(pc.receivedCandidates).toEqual(
      expect.arrayContaining([expect.objectContaining({ candidate: 'late-candidate' })])
    );
  });

  it('skips negotiation when an offer is already being handled (have-remote-offer race)', async () => {
    const { bus, signals } = makeManager('A');
    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    await flushMicrotasks();
    const pc = FakeRTCPeerConnection.instances[0];
    const offersBefore = signals.filter((s) => s.signal.sdp?.type === 'offer').length;

    // Remote offer arrived and is being answered — a track change fires
    // negotiationneeded in this state (the InvalidStateError race from logs).
    pc.signalingState = 'have-remote-offer';
    pc.addTrack(new FakeMediaStreamTrack('video'), new FakeMediaStream());
    await flushMicrotasks();

    const offersAfter = signals.filter((s) => s.signal.sdp?.type === 'offer').length;
    expect(offersAfter).toBe(offersBefore);
  });

  it('replaces tracks on all peer senders', async () => {
    const { pm, bus } = makeManager('A');
    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    await flushMicrotasks();
    const pc = FakeRTCPeerConnection.instances[0];

    const newTrack = new FakeMediaStreamTrack('audio');
    pm.replaceTrack('audio', newTrack);
    const audioSender = pc.senders.find((s) => s.track.kind === 'audio');
    expect(audioSender?.track).toBe(newTrack);
  });
});

describe('screen share audio (share audio toggle)', () => {
  it('sends screen audio and video tracks to existing peers', async () => {
    const { pm, bus } = makeManager('A');
    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    await flushMicrotasks();

    const screenAudio = new FakeMediaStreamTrack('audio');
    const screenVideo = new FakeMediaStreamTrack('video');
    pm.addScreenTrack(new FakeMediaStream([screenVideo, screenAudio]));
    await flushMicrotasks();

    const pc = FakeRTCPeerConnection.instances[0];
    expect(pc.senders.some((s) => s.track === screenAudio)).toBe(true);
    expect(pc.senders.some((s) => s.track === screenVideo)).toBe(true);
  });

  it('includes screen tracks on connections created after sharing started', async () => {
    const { pm, bus } = makeManager('A');
    const screenAudio = new FakeMediaStreamTrack('audio');
    const screenVideo = new FakeMediaStreamTrack('video');
    pm.addScreenTrack(new FakeMediaStream([screenVideo, screenAudio]));

    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    await flushMicrotasks();

    const pc = FakeRTCPeerConnection.instances[0];
    expect(pc.senders.some((s) => s.track === screenAudio)).toBe(true);
    expect(pc.senders.some((s) => s.track === screenVideo)).toBe(true);
  });

  it('removes screen tracks from all connections when sharing stops', async () => {
    const { pm, bus } = makeManager('A');
    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    await flushMicrotasks();

    const screenAudio = new FakeMediaStreamTrack('audio');
    pm.addScreenTrack(new FakeMediaStream([screenAudio]));
    await flushMicrotasks();
    pm.removeScreenTrack();

    const pc = FakeRTCPeerConnection.instances[0];
    expect(pc.senders.some((s) => s.track === screenAudio)).toBe(false);
  });

  it('attaches a screen audio track that arrives after sharing started (late display audio)', async () => {
    const { pm, bus } = makeManager('A');
    bus.emit(Events.PEER_JOINED, { id: 'B', username: 'Bob' });
    await flushMicrotasks();

    const listeners: Record<string, ((e: unknown) => void)[]> = {};
    const lateAudio = new FakeMediaStreamTrack('audio');
    const stream = {
      id: 'screen-1',
      getTracks: () => [] as FakeMediaStreamTrack[],
      getAudioTracks: () => [] as FakeMediaStreamTrack[],
      getVideoTracks: () => [] as FakeMediaStreamTrack[],
      addEventListener: (name: string, cb: (e: unknown) => void) => {
        (listeners[name] ||= []).push(cb);
      },
      removeEventListener: () => {},
    } as unknown as MediaStream;
    pm.addScreenTrack(stream);
    await flushMicrotasks();

    const pc = FakeRTCPeerConnection.instances[0];
    expect(pc.senders.some((s) => s.track === lateAudio)).toBe(false);

    // The browser delivers the audio track a moment later.
    listeners['addtrack']?.forEach((cb) => cb({ track: lateAudio }));
    await flushMicrotasks();
    expect(pc.senders.some((s) => s.track === lateAudio)).toBe(true);
  });
});
