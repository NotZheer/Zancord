import express from 'express';
import http from 'http';
import https from 'https';
import { Server } from 'socket.io';
import path from 'path';
import fs from 'fs';
import { fileURLToPath, pathToFileURL } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const DIST_DIR = path.join(__dirname, 'dist');

// ---------------------------------------------------------------------------
// Validation helpers (B1)
// ---------------------------------------------------------------------------

export function normalizeUsername(raw) {
  const str = String(raw ?? '')
    .trim()
    // eslint-disable-next-line no-control-regex
    .replace(/[\u0000-\u001f\u007f]/g, '');
  return str.slice(0, 24);
}

export function isValidRoomId(raw) {
  return typeof raw === 'string' && /^[a-zA-Z0-9_-]{1,64}$/.test(raw);
}

// ---------------------------------------------------------------------------
// Rate limiting (B2) — pure & unit-testable
// ---------------------------------------------------------------------------

export function checkRateLimit(tracker, type, maxPerWindow, windowMs = 1000) {
  const now = Date.now();
  if (!tracker[type]) tracker[type] = [];
  const timestamps = tracker[type];
  const valid = timestamps.filter((t) => now - t < windowMs);
  if (valid.length >= maxPerWindow) {
    tracker[type] = valid;
    return false;
  }
  valid.push(now);
  tracker[type] = valid;
  return true;
}

// ---------------------------------------------------------------------------
// App + Socket.io construction (import-safe: nothing listens on import)
// ---------------------------------------------------------------------------

const CSP = [
  "default-src 'self'",
  "script-src 'self' https:",
  "style-src 'self' 'unsafe-inline' https:",
  "font-src 'self' https: data:",
  "img-src 'self' data: blob:",
  "media-src 'self' blob:",
  "connect-src 'self' ws: wss: https: http:",
  "frame-ancestors 'none'",
].join('; ');

export function createApp() {
  const app = express();

  // Security headers (B6)
  app.use((req, res, next) => {
    res.setHeader('Content-Security-Policy', CSP);
    res.setHeader('X-Content-Type-Options', 'nosniff');
    res.setHeader('Referrer-Policy', 'no-referrer');
    res.setHeader('X-Frame-Options', 'DENY');
    next();
  });

  // Static hosting scoped to the built PWA only (C1) — never the repo root.
  app.use(
    express.static(DIST_DIR, {
      dotfiles: 'deny',
      index: false,
      setHeaders: (res, filePath) => {
        if (filePath.includes(`${path.sep}assets${path.sep}`)) {
          res.setHeader('Cache-Control', 'public, max-age=31536000, immutable');
        } else {
          res.setHeader('Cache-Control', 'no-cache');
        }
      },
    })
  );

  // SPA shell (C2)
  app.get('/', (req, res) => {
    res.sendFile(path.join(DIST_DIR, 'index.html'));
  });

  return app;
}

export function createSocketServer(httpServer) {
  const io = new Server(httpServer, {
    cors: {
      origin: '*',
      methods: ['GET', 'POST'],
    },
    pingTimeout: 10000,
    pingInterval: 5000,
  });

  // Rooms state: roomId -> Map<socketId, PeerInfo>
  const rooms = new Map();

  // Rate limiting trackers per socket
  const rateLimits = new Map();

  const limits = { signal: 30, chat: 5, state: 10, join: 3 };
  const joinWindowMs = 10000;

  const getTracker = (socketId) => {
    if (!rateLimits.has(socketId)) {
      rateLimits.set(socketId, { signal: [], chat: [], state: [], join: [] });
    }
    return rateLimits.get(socketId);
  };

  io.on('connection', (socket) => {
    console.log(`[SIGNALING] Socket connected: ${socket.id}`);
    let currentRoom = null;
    let currentUser = null;

    socket.on('join-room', (payload) => {
      const roomId = payload?.roomId;
      if (!isValidRoomId(roomId)) {
        console.warn(`[SIGNALING] Rejected join with invalid room id from ${socket.id}`);
        return;
      }

      // Join rate limit (B2)
      if (!checkRateLimit(getTracker(socket.id), 'join', limits.join, joinWindowMs)) {
        console.warn(`[SIGNALING] Join rate limit exceeded for ${socket.id}`);
        return;
      }

      if (!rooms.has(roomId)) {
        rooms.set(roomId, new Map());
      }

      const roomPeers = rooms.get(roomId);

      // Room limit check (max 6 peers)
      if (roomPeers.size >= 6) {
        console.warn(`[SIGNALING] Room ${roomId} full. Rejecting ${socket.id}`);
        socket.emit('room-full', { message: 'Room is full (max 6 peers)' });
        return;
      }

      currentRoom = roomId;
      currentUser = {
        id: socket.id,
        username: normalizeUsername(payload?.username) || 'User',
        isMuted: false,
        isCamOff: false,
        isScreenSharing: false,
      };

      // Send existing peers to new user
      const existingPeers = Array.from(roomPeers.values());
      socket.emit('room-users', { peers: existingPeers });

      // Add new user to room
      roomPeers.set(socket.id, currentUser);
      socket.join(roomId);

      // Notify existing peers in room
      socket.to(roomId).emit('user-joined', currentUser);
      console.log(
        `[SIGNALING] User ${currentUser.username} (${socket.id}) joined room ${roomId}. Total: ${roomPeers.size}`
      );
    });

    socket.on('signal', (payload) => {
      if (!checkRateLimit(getTracker(socket.id), 'signal', limits.signal)) {
        console.warn(`[SIGNALING] Rate limit exceeded for signal from ${socket.id}`);
        return;
      }
      const { targetId, signal } = payload || {};
      if (targetId && signal) {
        io.to(targetId).emit('signal', {
          senderId: socket.id,
          signal,
        });
      }
    });

    socket.on('send-chat-message', (payload) => {
      if (!checkRateLimit(getTracker(socket.id), 'chat', limits.chat)) {
        console.warn(`[SIGNALING] Rate limit exceeded for chat from ${socket.id}`);
        return;
      }
      if (!currentRoom || !currentUser) return;
      const text = typeof payload === 'string' ? payload : payload?.text;
      if (typeof text !== 'string' || !text.trim()) return;

      const chatMsg = {
        id: socket.id + '-' + Date.now().toString(36),
        peerId: socket.id,
        sender: currentUser.username,
        text: text.slice(0, 1000), // Max message length
        timestamp: Date.now(),
        isLocal: false,
      };

      io.to(currentRoom).emit('chat-message', chatMsg);
    });

    socket.on('delete-chat-message', (payload) => {
      if (!currentRoom || !currentUser) return;
      if (!checkRateLimit(getTracker(socket.id), 'chat', limits.chat)) return;
      const msgId = typeof payload === 'string' ? payload : payload?.id;
      if (typeof msgId !== 'string' || !msgId.trim()) return;
      socket.to(currentRoom).emit('chat-message-deleted', { id: msgId });
    });

    socket.on('clear-chat', () => {
      if (!currentRoom || !currentUser) return;
      socket.to(currentRoom).emit('chat-cleared');
    });

    socket.on('peer-state-changed', (state) => {
      if (!currentRoom || !currentUser) return;
      if (!checkRateLimit(getTracker(socket.id), 'state', limits.state)) {
        return;
      }
      const roomPeers = rooms.get(currentRoom);
      if (roomPeers && roomPeers.has(socket.id)) {
        const peer = roomPeers.get(socket.id);
        if (typeof state === 'object' && state !== null) {
          if (state.username !== undefined) {
            state = { ...state, username: normalizeUsername(state.username) || peer.username };
          }
          Object.assign(peer, state);
        }
        socket.to(currentRoom).emit('peer-state-changed', {
          peerId: socket.id,
          state,
        });
      }
    });

    const handleLeave = () => {
      rateLimits.delete(socket.id);
      if (currentRoom && rooms.has(currentRoom)) {
        const roomPeers = rooms.get(currentRoom);
        roomPeers.delete(socket.id);
        socket.to(currentRoom).emit('user-left', { id: socket.id });
        console.log(`[SIGNALING] Peer ${socket.id} left room ${currentRoom}. Remaining: ${roomPeers.size}`);

        if (roomPeers.size === 0) {
          rooms.delete(currentRoom);
          console.log(`[SIGNALING] Room ${currentRoom} empty, cleaned up.`);
        }
      }
      currentRoom = null;
      currentUser = null;
    };

    socket.on('leave-room', handleLeave);
    socket.on('disconnect', () => {
      console.log(`[SIGNALING] Socket disconnected: ${socket.id}`);
      handleLeave();
    });
  });

  return io;
}

// ---------------------------------------------------------------------------
// Entry point — only binds ports when run directly (node server.js)
// ---------------------------------------------------------------------------

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  const app = createApp();
  const httpServer = http.createServer(app);
  const io = createSocketServer(httpServer);

  const HTTP_PORT = process.env.PORT || 3000;
  httpServer.listen(HTTP_PORT, '0.0.0.0', () => {
    console.log(`[ZANCORD SIGNALING] HTTP Server running on port ${HTTP_PORT} (0.0.0.0)`);
  });

  // Create HTTPS server on 3443 if SSL certificates are present
  try {
    const keyPath = path.join(__dirname, 'key.pem');
    const certPath = path.join(__dirname, 'cert.pem');
    if (fs.existsSync(keyPath) && fs.existsSync(certPath)) {
      const options = {
        key: fs.readFileSync(keyPath),
        cert: fs.readFileSync(certPath),
      };
      const httpsServer = https.createServer(options, app);
      io.attach(httpsServer);
      const HTTPS_PORT = process.env.HTTPS_PORT || 3443;
      httpsServer.listen(HTTPS_PORT, () => {
        console.log(`[ZANCORD SIGNALING] HTTPS Server attached on port ${HTTPS_PORT}`);
      });
    } else {
      console.log('[ZANCORD SIGNALING] SSL certificates (key.pem, cert.pem) not found. HTTPS server on 3443 skipped.');
    }
  } catch (err) {
    console.warn('[ZANCORD SIGNALING] HTTPS server start skipped:', err.message);
  }
}
