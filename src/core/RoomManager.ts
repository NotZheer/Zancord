import { EventBus } from './EventBus';
import { Events, PeerInfo, ChatMessage } from '../types';

// Declare io global from socket.io CDN script
declare const io: any;

export class RoomManager {
  private socket: any = null;
  private currentRoomId: string | null = null;
  private currentUsername: string = 'User';
  private eventBus: EventBus;

  constructor(eventBus: EventBus) {
    this.eventBus = eventBus;
  }

  public getSocketId(): string | null {
    return this.socket ? this.socket.id : null;
  }

  public connect(serverUrl?: string): void {
    // PWA: the app is served from the same origin as the signaling server.
    const targetUrl = serverUrl || window.location.origin;

    console.log(`[ROOM] Connecting to signaling server at ${targetUrl}...`);

    if (typeof io === 'undefined') {
      console.error('[ROOM] Socket.io client script not loaded!');
      return;
    }

    this.socket = io(targetUrl, {
      transports: ['websocket', 'polling'],
      reconnection: true,
      reconnectionAttempts: Infinity,
      reconnectionDelay: 1000,
      reconnectionDelayMax: 5000,
    });

    this.socket.on('connect', () => {
      console.log(`[ROOM] Connected with socket ID ${this.socket.id}`);
      this.eventBus.emit(Events.SOCKET_CONNECTED, { socketId: this.socket.id });
    });

    this.socket.on('disconnect', (reason: string) => {
      console.warn(`[ROOM] Socket disconnected: ${reason}`);
      this.eventBus.emit(Events.SOCKET_DISCONNECTED, { reason });
    });

    this.socket.io.on('reconnect', () => {
      console.log('[ROOM] Socket reconnected!');
      if (this.currentRoomId) {
        console.log(`[ROOM] Re-joining room ${this.currentRoomId}...`);
        this.socket.emit('join-room', {
          roomId: this.currentRoomId,
          username: this.currentUsername,
        });
        this.eventBus.emit(Events.ROOM_JOINED, { roomId: this.currentRoomId });
      }
    });

    this.socket.on('room-users', (payload: { peers: PeerInfo[] }) => {
      console.log('[ROOM] Received room-users:', payload.peers);
      payload.peers.forEach((peer) => {
        if (peer.id !== this.socket.id) {
          this.eventBus.emit(Events.PEER_JOINED, { ...peer, isInitiator: true });
        }
      });
    });

    this.socket.on('user-joined', (peer: PeerInfo) => {
      console.log('[ROOM] User joined:', peer);
      if (peer.id !== this.socket.id) {
        this.eventBus.emit(Events.PEER_JOINED, { ...peer, isInitiator: false });
      }
    });

    this.socket.on('user-left', (payload: { id: string }) => {
      console.log('[ROOM] User left:', payload.id);
      this.eventBus.emit(Events.PEER_LEFT, { peerId: payload.id });
    });

    this.socket.on('signal', (data: { senderId: string; signal: any }) => {
      this.eventBus.emit('rtc-signal-received', data);
    });

    this.socket.on('chat-message', (msg: ChatMessage) => {
      msg.isLocal = msg.peerId === this.socket?.id;
      this.eventBus.emit(Events.CHAT_MESSAGE_RECEIVED, msg);
    });

    this.socket.on('chat-message-deleted', (data: { id: string }) => {
      this.eventBus.emit(Events.CHAT_MESSAGE_DELETED, data);
    });

    this.socket.on('chat-cleared', () => {
      this.eventBus.emit(Events.CHAT_CLEARED, {});
    });

    this.socket.on('peer-state-changed', (data: { peerId: string; state: Partial<PeerInfo> }) => {
      this.eventBus.emit(Events.PEER_STATE_CHANGED, data);
    });

    this.socket.on('room-full', (data: { message: string }) => {
      this.eventBus.emit(Events.TOAST, {
        message: data.message || 'Room is full',
        type: 'error',
        duration: 5000,
      });
    });
  }

  public joinRoom(roomId: string, username: string): void {
    this.currentRoomId = roomId;
    this.currentUsername = username;

    if (this.socket && this.socket.connected) {
      console.log(`[ROOM] Joining room "${roomId}" as "${username}"...`);
      this.socket.emit('join-room', { roomId, username });
      this.eventBus.emit(Events.ROOM_JOINED, { roomId });
    } else {
      this.eventBus.once(Events.SOCKET_CONNECTED, () => {
        console.log(`[ROOM] Deferred join for room "${roomId}"...`);
        this.socket.emit('join-room', { roomId, username });
        this.eventBus.emit(Events.ROOM_JOINED, { roomId });
      });
    }
  }

  public leaveRoom(): void {
    if (this.socket && this.currentRoomId) {
      console.log(`[ROOM] Leaving room "${this.currentRoomId}"...`);
      this.socket.emit('leave-room');
      this.eventBus.emit(Events.ROOM_LEFT, { roomId: this.currentRoomId });
      this.currentRoomId = null;
    }
  }

  public sendChatMessage(text: string): void {
    if (this.socket && this.currentRoomId) {
      this.socket.emit('send-chat-message', { text });
    }
  }

  public sendDeleteChatMessage(id: string): void {
    if (this.socket && this.currentRoomId) {
      this.socket.emit('delete-chat-message', { id });
    }
  }

  public sendClearChat(): void {
    if (this.socket && this.currentRoomId) {
      this.socket.emit('clear-chat');
    }
  }

  public sendSignal(targetId: string, signal: object): void {
    if (this.socket) {
      this.socket.emit('signal', { targetId, signal });
    }
  }

  public emitStateChange(state: Partial<PeerInfo>): void {
    if (this.socket && this.currentRoomId) {
      this.socket.emit('peer-state-changed', state);
    }
  }
}
