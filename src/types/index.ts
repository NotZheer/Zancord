// Event name constants — use const enum for zero-cost at runtime
export const enum Events {
  LOCAL_MEDIA_READY = 'local-media-ready',
  PEER_JOINED = 'peer-joined',
  PEER_LEFT = 'peer-left',
  PEER_STREAM_ADDED = 'peer-stream-added',
  PEER_STREAM_REMOVED = 'peer-stream-removed',
  PEER_STATE_CHANGED = 'peer-state-changed',
  SCREEN_SHARE_STARTED = 'screen-share-started',
  SCREEN_SHARE_STOPPED = 'screen-share-stopped',
  CHAT_MESSAGE_RECEIVED = 'chat-message-received',
  CHAT_MESSAGE_SENT = 'chat-message-sent',
  CHAT_MESSAGE_DELETED = 'chat-message-deleted',
  CHAT_CLEARED = 'chat-cleared',
  AUDIO_LEVEL = 'audio-level',
  CONNECTION_STATE_CHANGED = 'connection-state-changed',
  ROOM_JOINED = 'room-joined',
  ROOM_LEFT = 'room-left',
  SOCKET_CONNECTED = 'socket-connected',
  SOCKET_DISCONNECTED = 'socket-disconnected',
  TOAST = 'toast',
}

export interface Peer {
  id: string;
  username: string;
  connection: RTCPeerConnection;
  stream: MediaStream;
  iceCandidateQueue: RTCIceCandidate[];
  isMuted: boolean;
  isCamOff: boolean;
  isScreenSharing: boolean;
  connectionState: RTCIceConnectionState;
}

export interface PeerInfo {
  id: string;
  username: string;
  isMuted: boolean;
  isCamOff: boolean;
  isScreenSharing: boolean;
}

export interface ChatMessage {
  id: string;
  peerId: string;
  sender: string;
  text: string;
  timestamp: number;
  isLocal: boolean;
}

export interface TailscaleInfo {
  ip: string;
  status: string;
}

export interface AudioLevelData {
  target: 'local' | string; // 'local' or peerId
  level: number; // 0-100 normalized
}

export interface ToastOptions {
  message: string;
  type: 'info' | 'success' | 'warning' | 'error';
  duration?: number;
}

export interface MediaDevices {
  mics: MediaDeviceInfo[];
  cams: MediaDeviceInfo[];
  speakers: MediaDeviceInfo[];
}

/**
 * User-selected screen-share quality (PERF-AUDIT P1). `null` values mean
 * "source native / leave untouched" for that axis.
 */
export interface ScreenShareQuality {
  width: number | null;
  height: number | null;
  frameRate: number | null;
}

// Socket.io event payloads
export interface JoinRoomPayload {
  roomId: string;
  username: string;
}

export interface SignalPayload {
  targetId: string;
  signal: {
    sdp?: RTCSessionDescription;
    candidate?: RTCIceCandidate;
  };
}

export interface UserJoinedPayload {
  id: string;
  username: string;
  isMuted: boolean;
  isCamOff: boolean;
  isScreenSharing: boolean;
}

export interface RoomUsersPayload {
  peers: PeerInfo[];
}
