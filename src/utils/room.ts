// Room id resolution: the app defaults to ONE shared room so installed PWAs
// connect the moment both sides open them — no link exchange needed for the
// normal 2-person flow. A `#room=` link still overrides the default for
// guests or private rooms.

export const DEFAULT_ROOM_ID = 'zancord-room';

const ROOM_ID_MAX = 64;
const ROOM_CHARS = /[^a-zA-Z0-9_-]/g;

function sanitizeRoomId(raw: string): string {
  return raw.replace(ROOM_CHARS, '').slice(0, ROOM_ID_MAX);
}

/**
 * Resolve the room id from a location hash like `#room=my-room`.
 * Returns the shared default room when the hash has no usable room.
 */
export function resolveRoomId(hash: string): string {
  const cleaned = hash.replace(/^#/, '').trim();
  if (cleaned.startsWith('room=')) {
    const room = cleaned.slice('room='.length).trim();
    if (room) {
      const sanitized = sanitizeRoomId(room);
      if (sanitized) {
        return sanitized;
      }
    }
  }
  return DEFAULT_ROOM_ID;
}

/**
 * Build the shareable invite URL. In production the app is served from the
 * same origin as signaling (Tailscale Serve / direct IP), so the current
 * origin is the correct base. Dev mode (localhost) falls back to the
 * Tailscale IP + :3443.
 */
export function buildInviteUrl(baseOrigin: string, hostname: string, fallbackIp: string, roomId: string): string {
  const isLocal = hostname === 'localhost' || hostname === '127.0.0.1';
  if (!isLocal && baseOrigin) {
    return `${baseOrigin.replace(/\/$/, '')}/#room=${encodeURIComponent(roomId)}`;
  }
  return `https://${fallbackIp}:3443/#room=${encodeURIComponent(roomId)}`;
}
