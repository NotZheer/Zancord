import { describe, it, expect } from 'vitest';
import fs from 'fs';
import path from 'path';

describe('PWA manifest integrity (P1)', () => {
  const manifestPath = path.join(process.cwd(), 'public', 'manifest.json');
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));

  it('declares a standalone, installable app', () => {
    expect(manifest.name).toBeTruthy();
    expect(manifest.display).toBe('standalone');
    expect(manifest.start_url).toBe('/');
    expect(manifest.background_color).toBeTruthy();
  });

  it('references icons that actually exist', () => {
    expect(manifest.icons.length).toBeGreaterThanOrEqual(2);
    for (const icon of manifest.icons) {
      const iconPath = path.join(process.cwd(), 'public', icon.src.replace(/^\//, ''));
      expect(fs.existsSync(iconPath), `missing icon ${icon.src}`).toBe(true);
      expect(icon.sizes).toBeTruthy();
    }
  });

  it('has a service worker to register', () => {
    expect(fs.existsSync(path.join(process.cwd(), 'public', 'sw.js'))).toBe(true);
  });
});
