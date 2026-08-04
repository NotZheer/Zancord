/**
 * Minimal WebRTC fakes for unit tests.
 * PeerManager reads these from globalThis at call time, so tests can install
 * them before instantiating a manager.
 */

export class FakeMediaStreamTrack {
  public kind: string;
  public id: string;
  public enabled = true;
  private stopped = false;

  constructor(kind: string) {
    this.kind = kind;
    this.id = `track-${kind}-${Math.random().toString(36).slice(2, 8)}`;
  }

  public stop(): void {
    this.stopped = true;
  }
}

export class FakeMediaStream {
  public id: string;
  private tracks: FakeMediaStreamTrack[];

  constructor(tracks: FakeMediaStreamTrack[] = []) {
    this.id = `stream-${Math.random().toString(36).slice(2, 8)}`;
    this.tracks = [...tracks];
  }

  public getTracks(): FakeMediaStreamTrack[] {
    return [...this.tracks];
  }

  public getAudioTracks(): FakeMediaStreamTrack[] {
    return this.tracks.filter((t) => t.kind === 'audio');
  }

  public getVideoTracks(): FakeMediaStreamTrack[] {
    return this.tracks.filter((t) => t.kind === 'video');
  }

  public addTrack(track: FakeMediaStreamTrack): void {
    this.tracks.push(track);
  }

  public removeTrack(track: FakeMediaStreamTrack): void {
    this.tracks = this.tracks.filter((t) => t !== track);
  }
}

export class FakeRTCPeerConnection {
  public static instances: FakeRTCPeerConnection[] = [];

  public signalingState: string = 'stable';
  public iceConnectionState: string = 'new';
  public localDescription: { type: string; sdp: string } | null = null;
  public remoteDescription: { type: string; sdp: string } | null = null;
  public onicecandidate: ((event: { candidate: unknown }) => void) | null = null;
  public ontrack: ((event: { track: unknown; streams: unknown[] }) => void) | null = null;
  public onnegotiationneeded: (() => void) | null = null;
  public oniceconnectionstatechange: (() => void) | null = null;
  public senders: { track: FakeMediaStreamTrack; replaceTrack: (t: FakeMediaStreamTrack) => void }[] = [];
  public receivedCandidates: unknown[] = [];
  public closed = false;
  public restartCalled = false;

  constructor(public config: RTCConfiguration) {
    FakeRTCPeerConnection.instances.push(this);
  }

  public addTrack(track: FakeMediaStreamTrack, _stream: FakeMediaStream) {
    const sender = {
      track,
      replaceTrack: (t: FakeMediaStreamTrack) => {
        sender.track = t;
      },
    };
    this.senders.push(sender);
    // Browsers fire negotiationneeded async after track changes.
    queueMicrotask(() => this.onnegotiationneeded?.());
    return sender;
  }

  public removeTrack(sender: unknown): void {
    this.senders = this.senders.filter((s) => s !== sender);
  }

  public getSenders() {
    return this.senders;
  }

  public createOffer() {
    return Promise.resolve({ type: 'offer', sdp: `offer-sdp-${this.senders.length}` });
  }

  public createAnswer() {
    return Promise.resolve({ type: 'answer', sdp: `answer-sdp-${this.senders.length}` });
  }

  public async setLocalDescription(desc: { type: string; sdp?: string } | null): Promise<void> {
    if (!desc) return;
    if (desc.type === 'rollback') {
      this.localDescription = null;
      this.signalingState = 'stable';
      // Rollback triggers renegotiation per spec.
      queueMicrotask(() => this.onnegotiationneeded?.());
      return;
    }
    this.localDescription = { type: desc.type, sdp: desc.sdp ?? '' };
    this.signalingState = desc.type === 'offer' ? 'have-local-offer' : 'stable';
  }

  public async setRemoteDescription(desc: { type: string; sdp: string }): Promise<void> {
    this.remoteDescription = { type: desc.type, sdp: desc.sdp };
    this.signalingState = desc.type === 'offer' ? 'have-remote-offer' : 'stable';
  }

  public async addIceCandidate(candidate: unknown): Promise<void> {
    this.receivedCandidates.push(candidate);
  }

  public restartIce(): void {
    this.restartCalled = true;
  }

  public close(): void {
    this.closed = true;
  }
}

export class FakeRTCSessionDescription {
  public type: string;
  public sdp: string;

  constructor(init: { type: string; sdp?: string }) {
    this.type = init.type;
    this.sdp = init.sdp ?? '';
  }
}

export class FakeRTCIceCandidate {
  public candidate: string;
  public sdpMid: string | null = null;
  public sdpMLineIndex: number | null = null;

  constructor(init: { candidate: string; sdpMid?: string | null; sdpMLineIndex?: number | null }) {
    this.candidate = init.candidate;
    this.sdpMid = init.sdpMid ?? null;
    this.sdpMLineIndex = init.sdpMLineIndex ?? null;
  }
}

export function installFakeWebRTC(): void {
  (globalThis as any).MediaStream = FakeMediaStream;
  (globalThis as any).MediaStreamTrack = FakeMediaStreamTrack;
  (globalThis as any).RTCPeerConnection = FakeRTCPeerConnection;
  (globalThis as any).RTCSessionDescription = FakeRTCSessionDescription;
  (globalThis as any).RTCIceCandidate = FakeRTCIceCandidate;
  FakeRTCPeerConnection.instances = [];
}

export const flushMicrotasks = () => new Promise((resolve) => setTimeout(resolve, 10));
