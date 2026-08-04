import { describe, it, expect } from 'vitest';
import { resolveRoomId, buildInviteUrl, DEFAULT_ROOM_ID } from '../src/utils/room';

describe('resolveRoomId (shared default room)', () => {
  it('uses the shared default room when there is no hash — PWAs connect instantly', () => {
    expect(resolveRoomId('')).toBe(DEFAULT_ROOM_ID);
    expect(resolveRoomId('#')).toBe(DEFAULT_ROOM_ID);
  });

  it('parses a room from the URL hash', () => {
    expect(resolveRoomId('#room=my-room')).toBe('my-room');
    expect(resolveRoomId('#room=zancord-room')).toBe('zancord-room');
  });

  it('falls back to the default for whitespace or malformed hashes', () => {
    expect(resolveRoomId('#room=   ')).toBe(DEFAULT_ROOM_ID);
    expect(resolveRoomId('#foo')).toBe(DEFAULT_ROOM_ID);
    expect(resolveRoomId('#room=!!!')).toBe(DEFAULT_ROOM_ID);
  });

  it('sanitizes hostile input', () => {
    const room = resolveRoomId('#room=<script>alert(1)</script>');
    expect(room).not.toMatch(/[<>]/);
    expect(room.length).toBeGreaterThan(0);
  });

  it('caps room id length at 64 characters', () => {
    expect(resolveRoomId(`#room=${'x'.repeat(100)}`).length).toBe(64);
  });
});

describe('buildInviteUrl (B7)', () => {
  it('uses the current origin when accessed over a real hostname (ts.net or Tailscale IP)', () => {
    expect(buildInviteUrl('https://zans-mac.foo.ts.net', 'zans-mac.foo.ts.net', '100.1.2.3', 'zc-abc')).toBe(
      'https://zans-mac.foo.ts.net/#room=zc-abc'
    );
    expect(buildInviteUrl('https://100.107.100.95:3443', '100.107.100.95', '100.107.100.95', 'my-room')).toBe(
      'https://100.107.100.95:3443/#room=my-room'
    );
  });

  it('falls back to the Tailscale IP + :3443 when opened from localhost (dev mode)', () => {
    expect(buildInviteUrl('http://localhost:5173', 'localhost', '100.1.2.3', 'zc-abc')).toBe(
      'https://100.1.2.3:3443/#room=zc-abc'
    );
    expect(buildInviteUrl('http://127.0.0.1:5173', '127.0.0.1', '100.1.2.3', 'zc-abc')).toBe(
      'https://100.1.2.3:3443/#room=zc-abc'
    );
  });

  it('URL-encodes the room id', () => {
    expect(buildInviteUrl('https://x.ts.net', 'x.ts.net', '100.1.2.3', 'my room')).toBe(
      'https://x.ts.net/#room=my%20room'
    );
  });
});
