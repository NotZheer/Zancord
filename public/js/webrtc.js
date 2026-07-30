/**
 * ZanCord - Ultra High Performance WebRTC Engine
 * Robust Multi-Track Stream Classification & Instant Screen Share Sync
 */

class HavenWebRTC {
  constructor(socket, currentUser, options = {}) {
    this.socket = socket;
    this.currentUser = currentUser;
    
    this.localStream = null;
    this.screenStream = null;
    
    this.peers = new Map();
    
    this.audioCtx = null;
    this.localAnalyser = null;
    this.lowCutFilter = null;
    this.noiseGateCompressor = null;
    this.noiseSuppressionEnabled = true;
    this.noiseSensitivityDb = -45;

    this.currentMicDeviceId = null;
    this.currentCamDeviceId = null;

    this.rtcConfig = {
      iceServers: [
        { urls: 'stun:stun.l.google.com:19302' },
        { urls: 'stun:stun1.l.google.com:19302' },
        { urls: 'stun:stun2.l.google.com:19302' },
        { urls: 'stun:stun3.l.google.com:19302' },
        { urls: 'stun:stun4.l.google.com:19302' },
        { urls: 'stun:global.stun.twilio.com:3478' },
        { urls: 'stun:stun.cloudflare.com:3478' }
      ]
    };

    this.callbacks = {
      onRemoteStreamAdded: options.onRemoteStreamAdded || (() => {}),
      onRemoteScreenStreamAdded: options.onRemoteScreenStreamAdded || (() => {}),
      onRemoteScreenStreamRemoved: options.onRemoteScreenStreamRemoved || (() => {}),
      onRemoteStreamRemoved: options.onRemoteStreamRemoved || (() => {}),
      onChatMessageReceived: options.onChatMessageReceived || (() => {}),
      onPeerStateChanged: options.onPeerStateChanged || (() => {}),
      onAudioLevelUpdate: options.onAudioLevelUpdate || (() => {})
    };

    console.log('%c[ZANCORD INIT]%c Engine initialized for user:', 'color: #00f2fe; font-weight: bold;', 'color: #fff;', currentUser);
    this.initSocketListeners();
  }

  async getDevices() {
    if (!navigator.mediaDevices || !navigator.mediaDevices.enumerateDevices) {
      return { mics: [], cams: [], speakers: [] };
    }
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      return {
        mics: devices.filter(d => d.kind === 'audioinput'),
        cams: devices.filter(d => d.kind === 'videoinput'),
        speakers: devices.filter(d => d.kind === 'audiooutput')
      };
    } catch (err) {
      console.warn('Enumerate devices error:', err);
      return { mics: [], cams: [], speakers: [] };
    }
  }

  async initLocalMedia(videoEnabled = true, audioEnabled = true, micId = null, camId = null) {
    console.log('%c[MEDIA REQ]%c Requesting User Media (Cam: ' + videoEnabled + ', Mic: ' + audioEnabled + ')', 'color: #ffb703; font-weight: bold;', 'color: #fff;');
    
    if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
      console.error('[SECURE CONTEXT ERROR] Camera & Mic require HTTPS or localhost context.');
      if (!this.localStream) this.localStream = new MediaStream();
      return this.localStream;
    }

    try {
      const audioConstraints = audioEnabled ? {
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
        ...(micId ? { deviceId: { exact: micId } } : {})
      } : false;

      const videoConstraints = videoEnabled ? {
        width: { ideal: 1280 },
        height: { ideal: 720 },
        ...(camId ? { deviceId: { exact: camId } } : {})
      } : false;

      const stream = new MediaStream();

      // 1. Acquire Audio Track Independently
      if (audioEnabled) {
        try {
          const audioStream = await navigator.mediaDevices.getUserMedia({ audio: audioConstraints });
          audioStream.getAudioTracks().forEach(t => stream.addTrack(t));
          console.log('[AUDIO SUCCESS] Acquired audio track:', audioStream.getAudioTracks()[0].label);
        } catch (audioErr) {
          try {
            const basicAudio = await navigator.mediaDevices.getUserMedia({ audio: true });
            basicAudio.getAudioTracks().forEach(t => stream.addTrack(t));
            console.log('[AUDIO SUCCESS] Basic audio track acquired:', basicAudio.getAudioTracks()[0].label);
          } catch (basicAudioErr) {
            console.error('[AUDIO ERROR] Failed to acquire microphone audio:', basicAudioErr.name, basicAudioErr.message);
          }
        }
      }

      // 2. Acquire Video Track Independently
      if (videoEnabled) {
        try {
          const videoStream = await navigator.mediaDevices.getUserMedia({ video: videoConstraints });
          videoStream.getVideoTracks().forEach(t => stream.addTrack(t));
          console.log('[VIDEO SUCCESS] Acquired video track:', videoStream.getVideoTracks()[0].label);
        } catch (videoErr) {
          try {
            const basicVideo = await navigator.mediaDevices.getUserMedia({ video: true });
            basicVideo.getVideoTracks().forEach(t => stream.addTrack(t));
            console.log('[VIDEO SUCCESS] Basic video track acquired:', basicVideo.getVideoTracks()[0].label);
          } catch (basicVideoErr) {
            console.error('[VIDEO ERROR] Failed to acquire camera video:', basicVideoErr.name, basicVideoErr.message);
          }
        }
      }

      this.localStream = stream;
      this.setupAudioAnalyser(this.localStream);
      this.broadcastTracksToPeers();
      return this.localStream;
    } catch (err) {
      console.error('[MEDIA ERROR] General media initialization failure:', err);
      if (!this.localStream) this.localStream = new MediaStream();
      return this.localStream;
    }
  }

  broadcastTracksToPeers() {
    if (!this.localStream) return;
    for (const [peerId, peerObj] of this.peers.entries()) {
      const pc = peerObj.peerConnection;
      const senders = pc.getSenders();

      this.localStream.getTracks().forEach(async (track) => {
        const existingSender = senders.find(s => s.track && s.track.kind === track.kind);
        if (existingSender) {
          existingSender.replaceTrack(track);
        } else {
          pc.addTrack(track, this.localStream);
        }
      });
    }
  }

  toggleMicrophone(enabled) {
    if (this.localStream) {
      this.localStream.getAudioTracks().forEach(track => {
        track.enabled = enabled;
      });
    }
    this.socket.emit('user-state-change', { isMuted: !enabled });
  }

  toggleCamera(enabled) {
    if (this.localStream) {
      this.localStream.getVideoTracks().forEach(track => {
        track.enabled = enabled;
      });
    }
    this.socket.emit('user-state-change', { isCamOff: !enabled });
  }

  setupAudioAnalyser(stream) {
    const audioTrack = stream.getAudioTracks()[0];
    if (!audioTrack) return;

    try {
      const AudioContextClass = window.AudioContext || window.webkitAudioContext;
      if (!AudioContextClass) return;
      this.audioCtx = new AudioContextClass();
      
      const source = this.audioCtx.createMediaStreamSource(new MediaStream([audioTrack]));
      this.localAnalyser = this.audioCtx.createAnalyser();
      this.localAnalyser.fftSize = 64;

      source.connect(this.localAnalyser);
      const bufferLength = this.localAnalyser.frequencyBinCount;
      const dataArray = new Uint8Array(bufferLength);

      const updateVolume = () => {
        if (!this.localAnalyser) return;
        this.localAnalyser.getByteFrequencyData(dataArray);
        let sum = 0;
        for (let i = 0; i < bufferLength; i++) {
          sum += dataArray[i];
        }
        const average = sum / bufferLength;
        const normalizedLevel = Math.min(100, Math.round((average / 128) * 100));
        this.callbacks.onAudioLevelUpdate('local', normalizedLevel);
        requestAnimationFrame(updateVolume);
      };
      updateVolume();
    } catch (e) {
      console.warn('Audio Context setup failed:', e);
    }
  }

  joinRoom(roomId) {
    this.socket.emit('join-room', {
      roomId,
      username: this.currentUser.username,
      peerId: this.currentUser.id
    });
  }

  initSocketListeners() {
    this.socket.on('room-users', async ({ peers }) => {
      for (const peer of peers) {
        if (peer.id !== this.socket.id) {
          await this.createPeerConnection(peer.id, peer.username, true, peer.isScreenSharing);
        }
      }
    });

    this.socket.on('user-joined', async (user) => {
      await this.createPeerConnection(user.id, user.username, false, user.isScreenSharing);
    });

    this.socket.on('signal', async ({ senderId, signal }) => {
      let peerObj = this.peers.get(senderId);
      if (!peerObj) {
        peerObj = await this.createPeerConnection(senderId, 'User 2', false);
      }

      const pc = peerObj.peerConnection;

      if (signal.sdp) {
        try {
          const description = new RTCSessionDescription(signal.sdp);
          await pc.setRemoteDescription(description);

          if (peerObj.iceCandidateQueue && peerObj.iceCandidateQueue.length > 0) {
            for (const cand of peerObj.iceCandidateQueue) {
              await pc.addIceCandidate(cand);
            }
            peerObj.iceCandidateQueue = [];
          }

          if (description.type === 'offer') {
            const answer = await pc.createAnswer();
            await pc.setLocalDescription(answer);
            this.socket.emit('signal', {
              targetId: senderId,
              signal: { sdp: pc.localDescription }
            });
          }
        } catch (err) {
          console.warn('SDP Handled:', err.message);
        }
      } else if (signal.candidate) {
        try {
          const candidate = new RTCIceCandidate(signal.candidate);
          if (!pc.remoteDescription) {
            peerObj.iceCandidateQueue.push(candidate);
          } else {
            await pc.addIceCandidate(candidate);
          }
        } catch (err) {
          console.warn('ICE error:', err.message);
        }
      }
    });

    this.socket.on('peer-state-changed', ({ userId, isMuted, isCamOff, isScreenSharing, username }) => {
      const peerObj = this.peers.get(userId);
      if (peerObj) {
        if (username) peerObj.username = username;
        if (isMuted !== undefined) peerObj.isMuted = isMuted;
        if (isCamOff !== undefined) peerObj.isCamOff = isCamOff;
        if (isScreenSharing !== undefined) peerObj.isScreenSharing = isScreenSharing;
        this.callbacks.onPeerStateChanged(userId, peerObj);
      }
    });

    this.socket.on('user-left', ({ userId }) => {
      this.closePeerConnection(userId);
    });
  }

  async createPeerConnection(peerId, username, isInitiator, isScreenSharing = false) {
    const pc = new RTCPeerConnection(this.rtcConfig);

    const peerObj = {
      peerConnection: pc,
      stream: new MediaStream(),
      webcamStreamId: null,
      iceCandidateQueue: [],
      username,
      isMuted: false,
      isCamOff: false,
      isScreenSharing
    };

    this.peers.set(peerId, peerObj);

    if (this.localStream) {
      this.localStream.getTracks().forEach(track => {
        pc.addTrack(track, this.localStream);
      });
    }

    pc.onicecandidate = (event) => {
      if (event.candidate) {
        this.socket.emit('signal', {
          targetId: peerId,
          signal: { candidate: event.candidate }
        });
      }
    };

    pc.ontrack = (event) => {
      const remoteStream = event.streams[0];
      const track = event.track;

      if (track.kind === 'video') {
        if (!peerObj.stream.getTracks().some(t => t.id === track.id)) {
          peerObj.stream.addTrack(track);
        }
        this.callbacks.onRemoteStreamAdded(peerId, peerObj.stream, peerObj.username);
      } else if (track.kind === 'audio') {
        if (!peerObj.stream.getTracks().some(t => t.id === track.id)) {
          peerObj.stream.addTrack(track);
        }
        this.callbacks.onRemoteStreamAdded(peerId, peerObj.stream, peerObj.username);
      }
    };

    if (isInitiator) {
      try {
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        this.socket.emit('signal', {
          targetId: peerId,
          signal: { sdp: pc.localDescription }
        });
      } catch (err) {
        console.warn('Error creating offer:', err);
      }
    }

    return peerObj;
  }

  closePeerConnection(peerId) {
    const peerObj = this.peers.get(peerId);
    if (peerObj) {
      if (peerObj.peerConnection) {
        peerObj.peerConnection.close();
      }
      this.peers.delete(peerId);
      this.callbacks.onRemoteStreamRemoved(peerId);
    }
  }

  leaveRoom() {
    for (const peerId of this.peers.keys()) {
      this.closePeerConnection(peerId);
    }
    if (this.localStream) {
      this.localStream.getTracks().forEach(t => t.stop());
    }
  }
}
