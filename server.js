const express = require('express');
const http = require('http');
const https = require('https');
const fs = require('fs');
const path = require('path');
const { Server } = require('socket.io');

const app = express();

// Serve static frontend assets
app.use(express.static(path.join(__dirname, 'public')));

// Fallback route for SPA / room URLs
app.get('*', (req, res) => {
  res.sendFile(path.join(__dirname, 'public', 'index.html'));
});

// Ephemeral Room Store (In-Memory Only, Zero Logging, Zero DB)
const rooms = new Map();

// HTTP Server
const httpServer = http.createServer(app);

// HTTPS Server (Self-Signed TLS for Secure Context WebRTC)
let httpsServer = null;
const certPath = path.join(__dirname, 'certs', 'cert.pem');
const keyPath = path.join(__dirname, 'certs', 'key.pem');

if (fs.existsSync(certPath) && fs.existsSync(keyPath)) {
  const options = {
    key: fs.readFileSync(keyPath),
    cert: fs.readFileSync(certPath)
  };
  httpsServer = https.createServer(options, app);
}

// Socket.io attached to both HTTP and HTTPS
const io = new Server({
  cors: {
    origin: '*',
    methods: ['GET', 'POST']
  },
  pingTimeout: 60000,
  pingInterval: 25000,
  transports: ['websocket', 'polling']
});

io.attach(httpServer);
if (httpsServer) {
  io.attach(httpsServer);
}

io.on('connection', (socket) => {
  let currentRoom = null;
  let currentUser = { id: socket.id, username: 'Anonymous' };

  socket.on('join-room', ({ roomId, username }) => {
    if (!roomId) return;

    if (currentRoom) {
      socket.leave(currentRoom);
      const roomPeers = rooms.get(currentRoom);
      if (roomPeers) {
        roomPeers.delete(socket.id);
        if (roomPeers.size === 0) rooms.delete(currentRoom);
      }
    }

    currentRoom = roomId;
    currentUser.username = username || 'User';
    socket.join(roomId);

    if (!rooms.has(roomId)) {
      rooms.set(roomId, new Map());
    }
    const roomPeers = rooms.get(roomId);
    roomPeers.set(socket.id, {
      id: socket.id,
      username: currentUser.username,
      isMuted: false,
      isCamOff: false,
      isScreenSharing: false
    });

    const peersList = Array.from(roomPeers.values());
    socket.emit('room-users', { peers: peersList });

    socket.to(roomId).emit('user-joined', {
      id: socket.id,
      username: currentUser.username,
      isMuted: false,
      isCamOff: false,
      isScreenSharing: false
    });
  });

  socket.on('signal', ({ targetId, signal }) => {
    if (targetId && signal) {
      io.to(targetId).emit('signal', {
        senderId: socket.id,
        signal
      });
    }
  });

  socket.on('user-state-change', (data) => {
    if (currentRoom && rooms.has(currentRoom)) {
      const roomPeers = rooms.get(currentRoom);
      const userObj = roomPeers.get(socket.id);
      if (userObj) {
        Object.assign(userObj, data);
        socket.to(currentRoom).emit('peer-state-changed', {
          userId: socket.id,
          ...data
        });
      }
    }
  });

  socket.on('send-chat-message', ({ text }) => {
    if (currentRoom && text) {
      socket.to(currentRoom).emit('chat-message', {
        peerId: socket.id,
        sender: currentUser.username,
        text
      });
    }
  });

  socket.on('disconnect', (reason) => {
    if (currentRoom && rooms.has(currentRoom)) {
      const roomPeers = rooms.get(currentRoom);
      roomPeers.delete(socket.id);
      if (roomPeers.size === 0) {
        rooms.delete(currentRoom);
      } else {
        socket.to(currentRoom).emit('user-left', {
          userId: socket.id,
          reason
        });
      }
    }
  });
});

const HTTP_PORT = process.env.PORT || 3000;
const HTTPS_PORT = process.env.HTTPS_PORT || 3443;

httpServer.listen(HTTP_PORT, () => {
  console.log(`\n======================================================`);
  console.log(`🚀 ZanCord P2P Server is running!`);
  console.log(`🔒 Zero Telemetry | 100% Peer-to-Peer Encryption`);
  console.log(`🌐 HTTP URL:  http://localhost:${HTTP_PORT}`);
  if (httpsServer) {
    httpsServer.listen(HTTPS_PORT, () => {
      console.log(`🔒 HTTPS URL: https://localhost:${HTTPS_PORT} (SECURE CONTEXT FOR SAFARI/CHROME)`);
      console.log(`======================================================\n`);
    });
  } else {
    console.log(`======================================================\n`);
  }
});
