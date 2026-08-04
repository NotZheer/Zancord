import { EventBus } from './EventBus';
import { Events, Peer, PeerInfo } from '../types';

export class PeerManager {
  public peers: Map<string, Peer> = new Map();
  private localStream: MediaStream | null = null;
  private screenStream: MediaStream | null = null;
  private screenTracks: Set<MediaStreamTrack> = new Set();
  private localId: string | null = null;
  private eventBus: EventBus;
  private rtcConfig: RTCConfiguration = {
    iceServers: [], // No STUN/TURN — Tailscale handles direct P2P mesh
    iceCandidatePoolSize: 0,
    // P7: one bundled transport per peer (single DTLS/SRTP pair, fewer ICE
    // candidates) — the standard mesh recommendation. Cam/mic/screen tracks
    // share it, which is exactly what max-bundle is for.
    bundlePolicy: 'max-bundle',
  };

  constructor(eventBus: EventBus) {
    this.eventBus = eventBus;
    this.setupEventListeners();
  }

  public setLocalStream(stream: MediaStream): void {
    this.localStream = stream;
  }

  /**
   * Our own socket id, used to assign the polite/impolite role per pair.
   * Without a local id we default to polite (always accept remote offers).
   */
  public setLocalId(id: string): void {
    this.localId = id;
  }

  private isPolite(peerId: string): boolean {
    return this.localId === null ? true : this.localId < peerId;
  }

  private setupEventListeners(): void {
    this.eventBus.on<PeerInfo>(Events.PEER_JOINED, (peerInfo) => {
      console.log(`[PEER] PEER_JOINED handler: Creating connection to ${peerInfo.username} (${peerInfo.id})`);
      this.createPeerConnection(peerInfo.id, peerInfo.username);
    });

    this.eventBus.on<{ peerId: string }>(Events.PEER_LEFT, ({ peerId }) => {
      console.log(`[PEER] PEER_LEFT handler: Closing connection to ${peerId}`);
      this.closePeerConnection(peerId);
    });

    this.eventBus.on<{ senderId: string; signal: any }>('rtc-signal-received', ({ senderId, signal }) => {
      this.handleSignal(senderId, signal);
    });
  }

  public createPeerConnection(peerId: string, username: string): Peer {
    if (this.peers.has(peerId)) {
      console.warn(`[PEER] Connection to ${peerId} already exists. Returning existing.`);
      return this.peers.get(peerId)!;
    }

    console.log(`[PEER] Creating RTCPeerConnection for ${username} (${peerId})`);
    const pc = new RTCPeerConnection(this.rtcConfig);
    const remoteStream = new MediaStream();

    const peerObj: Peer = {
      id: peerId,
      username,
      connection: pc,
      stream: remoteStream,
      iceCandidateQueue: [],
      isMuted: false,
      isCamOff: false,
      isScreenSharing: false,
      connectionState: pc.iceConnectionState,
    };

    this.peers.set(peerId, peerObj);

    // Add local stream tracks to PC
    if (this.localStream) {
      this.localStream.getTracks().forEach((track) => {
        pc.addTrack(track, this.localStream!);
      });
    }

    // Add active screen tracks (video + optional audio) if present
    if (this.screenStream) {
      this.screenTracks.forEach((track) => {
        pc.addTrack(track, this.screenStream!);
      });
    }

    // ICE Candidate handler
    pc.onicecandidate = (event) => {
      if (event.candidate) {
        this.eventBus.emit('rtc-send-signal', {
          targetId: peerId,
          signal: { candidate: event.candidate },
        });
      }
    };

    let primaryStreamId: string | null = null;

    // Track handler
    pc.ontrack = (event) => {
      console.log(`[PEER] Received remote track (${event.track.kind}) from ${peerId}`);
      const incomingStream = event.streams[0] || new MediaStream([event.track]);

      if (!primaryStreamId) {
        primaryStreamId = incomingStream.id;
      }

      const isPrimaryStream = incomingStream.id === primaryStreamId;

      if (isPrimaryStream) {
        event.streams[0]?.getTracks().forEach((t) => {
          if (!remoteStream.getTracks().includes(t)) {
            remoteStream.addTrack(t);
          }
        });
        this.eventBus.emit(Events.PEER_STREAM_ADDED, {
          peerId,
          stream: remoteStream,
        });
      } else {
        const screenCardId = `${peerId}-screen`;
        console.log(
          `[PEER] Screen share stream received from ${peerId} (tracks: ${incomingStream.getTracks().map((t) => t.kind).join(', ') || 'none'})`
        );
        this.eventBus.emit('peer-screen-stream-added', {
          peerId: screenCardId,
          username: `${username}'s Screen`,
          stream: incomingStream,
        });

        event.track.onended = () => {
          console.log(`[PEER] Screen share track ended from ${peerId}`);
          this.eventBus.emit('peer-screen-stream-removed', { peerId: screenCardId });
        };
      }
    };

    // ICE Connection State Change handler
    pc.oniceconnectionstatechange = () => {
      console.log(`[PEER] Connection state for ${peerId}: ${pc.iceConnectionState}`);
      peerObj.connectionState = pc.iceConnectionState;

      this.eventBus.emit(Events.CONNECTION_STATE_CHANGED, {
        peerId,
        state: pc.iceConnectionState,
      });

      if (pc.iceConnectionState === 'failed') {
        console.warn(`[PEER] ICE connection failed for ${peerId}. Attempting restart...`);
        this.attemptIceRestart(peerId);
      } else if (pc.iceConnectionState === 'disconnected') {
        console.warn(`[PEER] ICE connection disconnected for ${peerId}. Waiting 5s...`);
        setTimeout(() => {
          if (peerObj.connection.iceConnectionState === 'disconnected') {
            console.warn(`[PEER] Still disconnected after 5s for ${peerId}. Attempting ICE restart...`);
            this.attemptIceRestart(peerId);
          }
        }, 5000);
      }
    };

    // Negotiation Needed — perfect negotiation (C3):
    // ANY peer may offer; glare is resolved by the polite/impolite role.
    makingOfferState.set(pc, { makingOffer: false });
    pc.onnegotiationneeded = async () => {
      const state = makingOfferState.get(pc);
      if (!state || state.makingOffer) return;
      // Skip while a remote offer is being handled — creating our own offer
      // then throws InvalidStateError ("Called in wrong state: have-remote-offer").
      if (pc.signalingState !== 'stable') {
        console.log(`[PEER] Skipping negotiation for ${peerId} (signalingState=${pc.signalingState})`);
        return;
      }
      try {
        state.makingOffer = true;
        console.log(`[PEER] Negotiation needed for ${peerId}. Creating offer...`);
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        this.eventBus.emit('rtc-send-signal', {
          targetId: peerId,
          signal: { sdp: pc.localDescription },
        });
      } catch (err) {
        console.error(`[PEER] Error during negotiation offer creation for ${peerId}:`, err);
      } finally {
        state.makingOffer = false;
      }
    };

    return peerObj;
  }

  public async handleSignal(
    senderId: string,
    signal: { sdp?: RTCSessionDescriptionInit; candidate?: RTCIceCandidateInit }
  ): Promise<void> {
    let peer = this.peers.get(senderId);

    if (!peer && signal.sdp?.type === 'offer') {
      console.log(`[PEER] Receiving offer signal from peer ${senderId}`);
      peer = this.createPeerConnection(senderId, 'Peer');
    }

    if (!peer) {
      console.warn(`[PEER] Received signal for unknown peer ${senderId} without offer.`);
      return;
    }

    const pc = peer.connection;

    try {
      if (signal.sdp) {
        // Perfect negotiation glare handling (C3)
        if (signal.sdp.type === 'offer') {
          const offerCollision = makingOfferRef(pc) || pc.signalingState !== 'stable';
          const ignoreOffer = !this.isPolite(senderId) && offerCollision;
          if (ignoreOffer) {
            console.log(`[PEER] Ignoring colliding offer from ${senderId} (impolite role)`);
            return;
          }
          if (offerCollision) {
            // Polite peer: roll back our own pending offer before accepting theirs.
            if (pc.signalingState !== 'stable') {
              console.log(`[PEER] Rolling back local offer to accept offer from ${senderId}`);
              await pc.setLocalDescription({ type: 'rollback' } as RTCSessionDescriptionInit);
            }
          }
        } else if (signal.sdp.type === 'answer' && pc.signalingState === 'stable') {
          // Unexpected answer (our offer was rolled back) — ignore.
          console.log(`[PEER] Ignoring unexpected answer from ${senderId}`);
          return;
        }

        console.log(`[PEER] Handling SDP ${signal.sdp.type} from ${senderId}`);
        await pc.setRemoteDescription(new RTCSessionDescription(signal.sdp));

        // Flush queued ICE candidates
        if (peer.iceCandidateQueue.length > 0) {
          console.log(`[PEER] Flushing ${peer.iceCandidateQueue.length} queued ICE candidates for ${senderId}`);
          for (const cand of peer.iceCandidateQueue) {
            await pc.addIceCandidate(cand);
          }
          peer.iceCandidateQueue = [];
        }

        if (signal.sdp.type === 'offer') {
          console.log(`[PEER] Creating answer for ${senderId}`);
          const answer = await pc.createAnswer();
          await pc.setLocalDescription(answer);
          this.eventBus.emit('rtc-send-signal', {
            targetId: senderId,
            signal: { sdp: pc.localDescription },
          });
        }
      } else if (signal.candidate) {
        const iceCandidate = new RTCIceCandidate(signal.candidate);
        if (pc.remoteDescription && pc.remoteDescription.type) {
          await pc.addIceCandidate(iceCandidate);
        } else {
          console.log(`[PEER] Queueing ICE candidate for ${senderId} (remote description not set)`);
          peer.iceCandidateQueue.push(iceCandidate);
        }
      }
    } catch (err) {
      console.error(`[PEER] Error handling signal from ${senderId}:`, err);
    }
  }

  private async attemptIceRestart(peerId: string): Promise<void> {
    const peer = this.peers.get(peerId);
    if (!peer) return;

    try {
      console.log(`[PEER] Restarting ICE for ${peerId}...`);
      const pc = peer.connection;
      if (pc.signalingState !== 'stable') {
        console.log(`[PEER] Skipping ICE restart for ${peerId} (signalingState=${pc.signalingState})`);
        return;
      }
      pc.restartIce();
      const offer = await pc.createOffer({ iceRestart: true });
      await pc.setLocalDescription(offer);
      this.eventBus.emit('rtc-send-signal', {
        targetId: peerId,
        signal: { sdp: pc.localDescription },
      });
    } catch (err) {
      console.error(`[PEER] ICE restart failed for ${peerId}:`, err);
    }
  }

  public replaceTrack(kind: 'audio' | 'video', newTrack: MediaStreamTrack): void {
    console.log(`[PEER] Replacing ${kind} track on all peer connections...`);
    this.peers.forEach((peer) => {
      const sender = peer.connection.getSenders().find((s) => s.track && s.track.kind === kind);
      if (sender) {
        sender.replaceTrack(newTrack);
      }
    });
  }

  /**
   * Send the full screen stream (video + optional audio) to all peers.
   * Late joiners get it too via createPeerConnection. Display-audio tracks
   * that arrive AFTER the picker closes are attached as they land.
   */
  public addScreenTrack(screenStream: MediaStream): void {
    this.screenStream = screenStream;
    this.screenTracks.clear();

    const attach = (track: MediaStreamTrack) => {
      if (this.screenTracks.has(track)) return;
      this.screenTracks.add(track);
      this.peers.forEach((peer) => {
        peer.connection.addTrack(track, screenStream);
      });
    };

    screenStream.getTracks().forEach(attach);

    if (typeof screenStream.addEventListener === 'function') {
      screenStream.addEventListener('addtrack', (e: Event) => {
        const track = (e as MediaStreamTrackEvent).track;
        console.log('[PEER] Screen stream gained a track mid-share; attaching...');
        attach(track);
      });
    }
  }

  public removeScreenTrack(): void {
    if (!this.screenStream) return;
    this.peers.forEach((peer) => {
      this.screenTracks.forEach((track) => {
        const sender = peer.connection.getSenders().find((s) => s.track === track);
        if (sender) {
          peer.connection.removeTrack(sender);
        }
      });
    });
    this.screenTracks.clear();
    this.screenStream = null;
  }

  public closePeerConnection(peerId: string): void {
    const peer = this.peers.get(peerId);
    if (peer) {
      console.log(`[PEER] Closing connection for ${peerId}`);
      peer.connection.close();
      peer.stream.getTracks().forEach((t) => t.stop());
      this.peers.delete(peerId);
    }
  }

  public closeAllConnections(): void {
    console.log('[PEER] Closing all peer connections...');
    this.peers.forEach((_, peerId) => {
      this.closePeerConnection(peerId);
    });
    this.peers.clear();
  }
}

// Per-connection "makingOffer" state, keyed by the underlying connection object.
// Kept module-local so handleSignal can consult it during glare resolution.
const makingOfferState = new WeakMap<RTCPeerConnection, { makingOffer: boolean }>();

function makingOfferRef(pc: RTCPeerConnection): boolean {
  return makingOfferState.get(pc)?.makingOffer ?? false;
}
