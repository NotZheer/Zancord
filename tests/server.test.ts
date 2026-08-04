import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import http, { AddressInfo } from 'http';
import { io as ioc, Socket } from 'socket.io-client';
import {
  createApp,
  createSocketServer,
  normalizeUsername,
  isValidRoomId,
  checkRateLimit,
} from '../server.js';

type TestServer = {
  httpServer: http.Server;
  url: string;
  close: () => Promise<void>;
};

async function startServer(): Promise<TestServer> {
  const app = createApp();
  const httpServer = http.createServer(app);
  createSocketServer(httpServer);
  await new Promise<void>((resolve) => httpServer.listen(0, resolve));
  const { port } = httpServer.address() as AddressInfo;
  return {
    httpServer,
    url: `http://127.0.0.1:${port}`,
    close: () =>
      new Promise<void>((resolve) => {
        // Force-close leaked keep-alive connections so the hook never hangs.
        httpServer.closeAllConnections?.();
        httpServer.close(() => resolve());
      }),
  };
}

async function connect(url: string): Promise<Socket> {
  const socket = ioc(url, { transports: ['websocket'], forceNew: true, reconnection: false });
  await new Promise<void>((resolve, reject) => {
    socket.on('connect', () => resolve());
    socket.on('connect_error', reject);
  });
  return socket;
}

function waitFor<T = any>(socket: Socket, event: string, timeoutMs = 2000): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`Timed out waiting for "${event}"`)), timeoutMs);
    socket.once(event, (data: T) => {
      clearTimeout(timer);
      resolve(data);
    });
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function get(url: string, path: string) {
  return new Promise<{ status: number; headers: http.IncomingHttpHeaders; body: string }>(
    (resolve, reject) => {
      const req = http.get(`${url}${path}`, (res) => {
        let body = '';
        res.on('data', (c) => (body += c));
        res.on('end', () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body }));
      });
      req.on('error', reject);
    }
  );
}

describe('normalizeUsername (B1)', () => {
  it('trims whitespace', () => {
    expect(normalizeUsername('  Bob  ')).toBe('Bob');
  });

  it('caps length at 24 characters', () => {
    expect(normalizeUsername('x'.repeat(50)).length).toBe(24);
  });

  it('strips control characters', () => {
    expect(normalizeUsername('\u0000ab\u001fcd\u007f')).toBe('abcd');
  });

  it('falls back to empty string for null/undefined', () => {
    expect(normalizeUsername(null)).toBe('');
    expect(normalizeUsername(undefined)).toBe('');
  });
});

describe('isValidRoomId (B1)', () => {
  it('accepts alphanumeric, dash, underscore', () => {
    expect(isValidRoomId('zancord-room')).toBe(true);
    expect(isValidRoomId('zc-Ab_12')).toBe(true);
  });

  it('rejects empty, whitespace, symbols, and overlong ids', () => {
    expect(isValidRoomId('')).toBe(false);
    expect(isValidRoomId('has space')).toBe(false);
    expect(isValidRoomId('emoji😀')).toBe(false);
    expect(isValidRoomId('x'.repeat(65))).toBe(false);
    expect(isValidRoomId(undefined)).toBe(false);
  });
});

describe('checkRateLimit (B2)', () => {
  it('allows up to maxPerSec then rejects within the window', () => {
    const tracker: Record<string, number[]> = {};
    expect(checkRateLimit(tracker, 'join', 3, 10000)).toBe(true);
    expect(checkRateLimit(tracker, 'join', 3, 10000)).toBe(true);
    expect(checkRateLimit(tracker, 'join', 3, 10000)).toBe(true);
    expect(checkRateLimit(tracker, 'join', 3, 10000)).toBe(false);
  });

  it('tracks signal and chat independently', () => {
    const tracker: Record<string, number[]> = {};
    expect(checkRateLimit(tracker, 'signal', 2)).toBe(true);
    expect(checkRateLimit(tracker, 'signal', 2)).toBe(true);
    expect(checkRateLimit(tracker, 'signal', 2)).toBe(false);
    expect(checkRateLimit(tracker, 'chat', 2)).toBe(true);
  });
});

describe('static serving & headers (C1, C2, B6)', () => {
  let server: TestServer;
  beforeEach(async () => {
    server = await startServer();
  });
  afterEach(async () => {
    await server.close();
  });

  it('does NOT serve the project root (private keys, source, package.json)', async () => {
    const forbidden = ['/key.pem', '/cert.pem', '/package.json', '/server.js', '/src/main.ts'];
    for (const p of forbidden) {
      const res = await get(server.url, p);
      expect(res.status, p).toBe(404);
    }
  });

  it('serves the built app shell at /', async () => {
    const res = await get(server.url, '/');
    expect(res.status).toBe(200);
    expect(res.body).toContain('id="app"');
    // The dev entry references raw TS — the built app must not.
    expect(res.body).not.toContain('/src/main.ts');
  });

  it('serves PWA assets (manifest + service worker)', async () => {
    const manifest = await get(server.url, '/manifest.json');
    expect(manifest.status).toBe(200);
    const sw = await get(server.url, '/sw.js');
    expect(sw.status).toBe(200);
  });

  it('sends security headers', async () => {
    const res = await get(server.url, '/');
    expect(res.headers['content-security-policy']).toBeTruthy();
    expect(res.headers['x-content-type-options']).toBe('nosniff');
    expect(res.headers['referrer-policy']).toBeTruthy();
  });
});

describe('socket.io room lifecycle (B1, B2, B4)', () => {
  let server: TestServer;
  beforeEach(async () => {
    server = await startServer();
  });
  afterEach(async () => {
    await server.close();
  });

  it('normalizes usernames in join payloads', async () => {
    const a = await connect(server.url);
    const b = await connect(server.url);
    a.emit('join-room', { roomId: 'room-x', username: '   Alice   ' });
    b.emit('join-room', { roomId: 'room-x', username: 'B' });
    const roomUsers = await waitFor<{ peers: { username: string }[] }>(b, 'room-users');
    expect(roomUsers.peers[0].username).toBe('Alice');
    a.close();
    b.close();
  });

  it('rejects invalid room ids without crashing or admitting the user', async () => {
    const a = await connect(server.url);
    const b = await connect(server.url);
    let joined = false;
    a.on('user-joined', () => (joined = true));
    a.emit('join-room', { roomId: 'valid-room', username: 'A' });
    await sleep(100);
    b.emit('join-room', { roomId: 'bad room!', username: 'B' });
    await sleep(300);
    expect(joined).toBe(false);
    a.close();
    b.close();
  });

  it('enforces the 6-peer room limit', async () => {
    const sockets: Socket[] = [];
    for (let i = 0; i < 6; i++) {
      const s = await connect(server.url);
      sockets.push(s);
      s.emit('join-room', { roomId: 'full-room', username: `U${i}` });
      await sleep(30);
    }
    const seventh = await connect(server.url);
    seventh.emit('join-room', { roomId: 'full-room', username: 'U7' });
    const full = await waitFor<{ message: string }>(seventh, 'room-full');
    expect(full.message).toContain('full');
    sockets.forEach((s) => s.close());
    seventh.close();
  });

  it('broadcasts user-left on disconnect', async () => {
    const a = await connect(server.url);
    const b = await connect(server.url);
    a.emit('join-room', { roomId: 'leavers', username: 'A' });
    b.emit('join-room', { roomId: 'leavers', username: 'B' });
    await sleep(100);
    const leftPromise = waitFor<{ id: string }>(a, 'user-left');
    b.close();
    const left = await leftPromise;
    expect(left.id).toBeTruthy();
    a.close();
  });

  it('rate-limits join-room per socket (3 per 10s)', async () => {
    const a = await connect(server.url);
    for (const room of ['r1', 'r2', 'r3', 'r4']) {
      a.emit('join-room', { roomId: room, username: 'A' });
    }
    await sleep(150);
    const c = await connect(server.url);
    c.emit('join-room', { roomId: 'r4', username: 'C' });
    const roomUsers = await waitFor<{ peers: unknown[] }>(c, 'room-users');
    expect(roomUsers.peers).toHaveLength(0);
    a.close();
    c.close();
  });
});

describe('chat (B1, B2)', () => {
  let server: TestServer;
  beforeEach(async () => {
    server = await startServer();
  });
  afterEach(async () => {
    await server.close();
  });

  it('truncates chat messages to 1000 characters', async () => {
    const a = await connect(server.url);
    const b = await connect(server.url);
    a.emit('join-room', { roomId: 'chat-room', username: 'A' });
    b.emit('join-room', { roomId: 'chat-room', username: 'B' });
    await sleep(100);
    a.emit('send-chat-message', { text: 'x'.repeat(1500) });
    const msg = await waitFor<{ text: string }>(b, 'chat-message');
    expect(msg.text.length).toBe(1000);
    a.close();
    b.close();
  });

  it('rate-limits chat to 5 messages per second', async () => {
    const a = await connect(server.url);
    const b = await connect(server.url);
    a.emit('join-room', { roomId: 'chat-room', username: 'A' });
    b.emit('join-room', { roomId: 'chat-room', username: 'B' });
    await sleep(100);
    let received = 0;
    b.on('chat-message', () => received++);
    for (let i = 0; i < 8; i++) {
      a.emit('send-chat-message', { text: `msg-${i}` });
    }
    await sleep(400);
    expect(received).toBe(5);
    a.close();
    b.close();
  });
});

describe('signal relay (B2)', () => {
  let server: TestServer;
  beforeEach(async () => {
    server = await startServer();
  });
  afterEach(async () => {
    await server.close();
  });

  it('rate-limits signal relay to 30 per second', async () => {
    const a = await connect(server.url);
    const b = await connect(server.url);
    a.emit('join-room', { roomId: 'sig-room', username: 'A' });
    b.emit('join-room', { roomId: 'sig-room', username: 'B' });
    await sleep(100);
    let received = 0;
    b.on('signal', () => received++);
    for (let i = 0; i < 35; i++) {
      a.emit('signal', { targetId: b.id, signal: { candidate: { candidate: `c-${i}` } } });
    }
    await sleep(400);
    expect(received).toBe(30);
    a.close();
    b.close();
  });
});
