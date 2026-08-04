/**
 * ZanCord PWA icon generator (P1).
 * Produces installable PNG icons with zero dependencies:
 *   public/icon-192.png, public/icon-512.png,
 *   public/icon-512-maskable.png, public/apple-touch-icon.png
 *
 * Design: dark rounded-square background + cyan lightning bolt.
 * Run: npm run icons
 */
import zlib from 'zlib';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const OUT_DIR = path.join(__dirname, '..', 'public');

// Palette
const BG = [11, 11, 13]; // #0b0b0d
const BOLT = [0, 242, 254]; // #00f2fe

// Lightning bolt polygon (normalized 0..1 coordinates)
const BOLT_POLY = [
  [0.62, 0.08],
  [0.28, 0.54],
  [0.46, 0.54],
  [0.38, 0.92],
  [0.74, 0.44],
  [0.54, 0.44],
];

// ---------------------------------------------------------------------------
// Minimal PNG encoder (RGBA8, no filters)
// ---------------------------------------------------------------------------

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(buf) {
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    crc = CRC_TABLE[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const typeBuf = Buffer.from(type, 'ascii');
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])));
  return Buffer.concat([len, typeBuf, data, crc]);
}

function encodePNG(size, pixelFn) {
  const stride = 1 + size * 4;
  const raw = Buffer.alloc(size * stride);
  for (let y = 0; y < size; y++) {
    raw[y * stride] = 0; // filter: none
    for (let x = 0; x < size; x++) {
      const [r, g, b, a] = pixelFn(x, y);
      const off = y * stride + 1 + x * 4;
      raw[off] = r;
      raw[off + 1] = g;
      raw[off + 2] = b;
      raw[off + 3] = a;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type: RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', zlib.deflateSync(raw, { level: 9 })),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

// ---------------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------------

function inPolygon(px, py, poly) {
  let inside = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const [xi, yi] = poly[i];
    const [xj, yj] = poly[j];
    const intersect =
      yi > py !== yj > py && px < ((xj - xi) * (py - yi)) / (yj - yi) + xi;
    if (intersect) inside = !inside;
  }
  return inside;
}

function inRoundedRect(x, y, size, radius) {
  const r = radius * size;
  const cx = Math.min(Math.max(x, r), size - r);
  const cy = Math.min(Math.max(y, r), size - r);
  return (x - cx) ** 2 + (y - cy) ** 2 <= r * r;
}

/**
 * @param size icon size in px
 * @param opts.maskable full-bleed background (no rounded corners)
 * @param opts.boltScale relative size of the bolt (0..1)
 */
function makeIcon(size, { maskable = false, boltScale = 1 } = {}) {
  const cornerRadius = maskable ? 0 : 0.22;
  const boltCenter = 0.5;
  const boltPoly = BOLT_POLY.map(([x, y]) => [
    boltCenter + (x - boltCenter) * boltScale,
    boltCenter + (y - boltCenter) * boltScale,
  ]);

  return encodePNG(size, (x, y) => {
    const nx = (x + 0.5) / size;
    const ny = (y + 0.5) / size;
    const inBg = maskable ? true : inRoundedRect(x, y, size, cornerRadius);
    if (!inBg) return [0, 0, 0, 0];
    if (inPolygon(nx, ny, boltPoly)) return [...BOLT, 255];
    return [...BG, 255];
  });
}

// ---------------------------------------------------------------------------

fs.mkdirSync(OUT_DIR, { recursive: true });

const outputs = [
  ['icon-192.png', makeIcon(192)],
  ['icon-512.png', makeIcon(512)],
  ['icon-512-maskable.png', makeIcon(512, { maskable: true, boltScale: 0.62 })],
  ['apple-touch-icon.png', makeIcon(180, { boltScale: 0.85 })],
];

for (const [name, buffer] of outputs) {
  fs.writeFileSync(path.join(OUT_DIR, name), buffer);
  console.log(`[ICONS] wrote public/${name} (${buffer.length} bytes)`);
}
