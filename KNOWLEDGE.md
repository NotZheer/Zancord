# ZanCord — Technical Knowledge Base

> ⚡ **PWA pivot (2026-08-01):** Tauri/desktop-shell sections (permissions, WebKitGTK, ScreenCaptureKit, entitlements, Tauri config) are **historical**. The app is now a browser PWA served from `server.js` (see `AGENT.md` amendment + `AUDIT.md`).

> Cross-platform Tauri v2 + WebRTC reference. Read this when you need to understand
> **why** something works (or doesn't work) on a specific platform.

---

## Table of Contents
1. [Tauri v2 Overview & Maturity](#1-tauri-v2-overview--maturity)
2. [Platform WebView Engines](#2-platform-webview-engines)
3. [WebRTC Support Matrix](#3-webrtc-support-matrix)
4. [Camera & Mic Permissions — Per Platform](#4-camera--mic-permissions--per-platform)
5. [Screen Sharing — The Hard Problem](#5-screen-sharing--the-hard-problem)
6. [Tailscale Integration Patterns](#6-tailscale-integration-patterns)
7. [Web Audio API — Noise Gate & HPF](#7-web-audio-api--noise-gate--hpf)
8. [WebRTC Mesh Topology & Limits](#8-webrtc-mesh-topology--limits)
9. [Build Dependencies Per Platform](#9-build-dependencies-per-platform)
10. [Known Bugs & Workarounds](#10-known-bugs--workarounds)
11. [Tauri v2 Configuration Reference](#11-tauri-v2-configuration-reference)

---

## 1. Tauri v2 Overview & Maturity

- **Stable release**: October 2, 2024 (v2.0.0). As of mid-2026, Tauri v2 is mature and actively maintained.
- **Key v2 change**: Replaced v1's "allowlist" with an **ACL & Capabilities** permission system. Permissions are declared in `src-tauri/capabilities/*.json` instead of `tauri.conf.json` allowlist.
- **Architecture**: Tauri does NOT bundle Chromium (unlike Electron). It uses the OS-native webview:
  - Windows → **WebView2** (Edge/Chromium engine)
  - macOS → **WKWebView** (Apple WebKit engine)
  - Linux → **WebKitGTK** (GNOME WebKit engine)
- **Implication**: Web API behavior varies across platforms because each webview engine has different capabilities. This is the fundamental tradeoff vs Electron.

---

## 2. Platform WebView Engines

| Platform | Engine | WebRTC Quality | Screen Share | Media Permissions |
|----------|--------|---------------|-------------|-------------------|
| **Windows 11** | WebView2 (Chromium) | ⭐ Excellent | ✅ Native JS | Auto-prompt |
| **macOS** | WKWebView | ⚠️ Good (with config) | ❌ Broken in WKWebView | Requires Info.plist + Entitlements |
| **Linux (Zorin/Ubuntu)** | WebKitGTK 4.1 | ⚠️ Fragile | ❌ Broken in WebKitGTK | Requires Rust hooks + GStreamer |

### Windows 11 — WebView2
- **Best platform for WebRTC.** WebView2 is literally Chromium, so all standard WebRTC APIs work identically to Chrome.
- WebView2 Evergreen Runtime is **pre-installed** on Windows 11 and auto-updates.
- On Windows 10, Tauri's NSIS installer automatically installs WebView2 if missing.
- Permission prompts appear as standard Chromium-style infobar prompts.

### macOS — WKWebView
- WKWebView supports `getUserMedia` for camera/mic but **requires explicit entitlements and Info.plist descriptions**.
- Without proper code signing + entitlements, `getUserMedia` silently fails or throws `NotAllowedError`.
- `getDisplayMedia` is **broken/restricted** — WKWebView does not provide a screen picker UI.
- If a user clicks "Don't Allow" on the macOS system prompt, WKWebView **cannot re-trigger it**. The user must manually go to `System Settings > Privacy & Security > Camera/Microphone`.
- The `macos-private-api` Tauri flag can unlock some hidden WebKit features but **disqualifies the app from Mac App Store** (not relevant for ZanCord since it's not going on the App Store).

### Linux — WebKitGTK
- **Tauri v2 requires `libwebkit2gtk-4.1`** (linked against `libsoup3`). Do NOT use `webkit2gtk-4.0` (that's v1).
- WebKitGTK delegates media processing to **GStreamer**. Missing GStreamer plugins = silent failures.
- `getUserMedia` works BUT requires manual Rust-side permission grants:
  ```rust
  // In Tauri setup, inside with_webview:
  use webkit2gtk::{SettingsExt, WebViewExt, PermissionRequestExt};

  let wv = webview.inner();

  // Enable WebRTC and media streams in WebKitGTK settings
  if let Some(settings) = wv.settings() {
      settings.set_enable_webrtc(true);
      settings.set_enable_media_stream(true);
  }

  // Auto-grant camera/mic permission requests
  wv.connect_permission_request(|_, req| {
      req.allow();
      true
  });
  ```
- `getDisplayMedia` is broken — WebKitGTK has no native screen picker dialog. Screen capture on Linux requires XDG Desktop Portal + PipeWire pipeline.

---

## 3. WebRTC Support Matrix

### APIs That Work Everywhere
| API | Windows | macOS | Linux | Notes |
|-----|---------|-------|-------|-------|
| `RTCPeerConnection` | ✅ | ✅ | ✅ | Core P2P connection — universal |
| `RTCSessionDescription` | ✅ | ✅ | ✅ | SDP offer/answer |
| `RTCIceCandidate` | ✅ | ✅ | ✅ | ICE negotiation |
| `MediaStream` | ✅ | ✅ | ✅ | Stream container |
| `getUserMedia` (cam/mic) | ✅ | ⚠️ | ⚠️ | macOS needs entitlements, Linux needs GStreamer + Rust hooks |
| `getDisplayMedia` (screen) | ✅ | ❌ | ❌ | Only works in Chromium-based webviews |
| `enumerateDevices` | ✅ | ✅ | ⚠️ | Linux may return empty labels without PipeWire permissions |
| `MediaRecorder` | ✅ | ✅ | ⚠️ | Linux depends on GStreamer codec support |

### ICE Configuration for Tailscale
Since all peers are on the same Tailnet, the ICE config should be minimal:
```typescript
const rtcConfig: RTCConfiguration = {
  iceServers: [],  // No STUN/TURN needed — Tailscale provides direct connectivity
  iceCandidatePoolSize: 0,
  iceTransportPolicy: 'all'
};
```
Peers will discover each other via **host candidates** using their Tailscale `100.x.x.x` IPs. The WebRTC ICE agent will find these addresses through the OS network interface list, which includes the Tailscale virtual interface (`tailscale0` / `utun*`).

**Important**: If you see ICE failures, it may be because the WebView's ICE agent is filtering out the Tailscale interface. In that case, consider passing the Tailscale IP explicitly through the signaling server and using it as a relay candidate hint.

---

## 4. Camera & Mic Permissions — Per Platform

### Windows 11
- WebView2 shows a standard Chromium permission infobar when `getUserMedia` is called.
- System-level privacy toggle: `Settings > Privacy & Security > Camera / Microphone > Allow desktop apps to access your camera`. If this is OFF, all `getUserMedia` calls fail with `NotAllowedError`.
- **Cached denial**: If denied in WebView2, the state is cached in `%LOCALAPPDATA%\<app>\EBWebView\Default\`. Clearing this folder resets it.

### macOS (Sonoma / Sequoia)
Required files:

**`src-tauri/Info.plist`**:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>NSCameraUsageDescription</key>
    <string>ZanCord requires camera access for P2P video calls.</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>ZanCord requires microphone access for P2P voice calls.</string>
    <key>NSDesktopScreenRecordingUsageDescription</key>
    <string>ZanCord requires screen capture permission to share your screen.</string>
</dict>
</plist>
```

**`src-tauri/Entitlements.plist`**:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.device.camera</key>
    <true/>
    <key>com.apple.security.device.microphone</key>
    <true/>
</dict>
</plist>
```

**`tauri.conf.json`** bundle section:
```json
{
  "bundle": {
    "macOS": {
      "entitlements": "Entitlements.plist",
      "signingIdentity": "-"
    }
  }
}
```

### Linux (Zorin OS / Ubuntu)
Rust-side setup in `main.rs` (already partially implemented in current codebase):
```rust
#[cfg(target_os = "linux")]
if let Some(window) = app.get_webview_window("main") {
    let _ = window.with_webview(|webview| {
        use webkit2gtk::{SettingsExt, WebViewExt, PermissionRequestExt};

        let wv = webview.inner();

        // Enable WebRTC engine
        if let Some(settings) = wv.settings() {
            settings.set_enable_webrtc(true);
            settings.set_enable_media_stream(true);
            settings.set_enable_media_capabilities(true);
        }

        // Auto-grant all permission requests (camera, mic)
        wv.connect_permission_request(|_, req| {
            println!("[WEBKIT PERMISSION] Auto-granting permission request");
            req.allow();
            true
        });
    });
}
```

Required GStreamer plugins for WebRTC audio/video encoding:
```bash
sudo apt install -y \
  gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly \
  gstreamer1.0-libav
```

---

## 5. Screen Sharing — The Hard Problem

### Why It's Hard
`navigator.mediaDevices.getDisplayMedia()` only works reliably in **Chromium-based** webviews (WebView2 on Windows). Apple's WKWebView and GNOME's WebKitGTK both lack the native screen picker UI that Chrome provides.

### Solution Strategy: Hybrid Architecture

```
┌─────────────────────────────────────────────────┐
│                  Screen Share Flow               │
│                                                  │
│  ┌─── Windows ───┐  ┌── macOS ──┐  ┌── Linux ──┐│
│  │ JS getDisplay  │  │ Rust      │  │ Rust      ││
│  │ Media() works  │  │ ScreenKit │  │ PipeWire  ││
│  │ natively       │  │ capture   │  │ / xcap    ││
│  └───────┬───────┘  └─────┬─────┘  └─────┬─────┘│
│          │                │               │      │
│          └────────┬───────┴───────┬───────┘      │
│                   ▼               ▼              │
│          WebRTC addTrack()  or  Canvas stream    │
│                   │                              │
│                   ▼                              │
│          Broadcast to all peers via mesh         │
└─────────────────────────────────────────────────┘
```

#### Windows Implementation
```typescript
// Direct JS — works perfectly in WebView2
async function startScreenShare(): Promise<MediaStream> {
  return navigator.mediaDevices.getDisplayMedia({
    video: {
      width: { ideal: 1920 },
      height: { ideal: 1080 },
      frameRate: { ideal: 60 }
    },
    audio: true // System audio capture
  });
}
```

#### macOS Implementation (Rust-side)
Use `ScreenCaptureKit` (Apple's native API available since macOS 12.3):
```rust
// Tauri IPC command
#[tauri::command]
async fn start_native_screen_capture() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // Use screencapturekit-rs or objc bindings
        // Capture frames → encode to H264 → pipe to frontend via IPC
        // Frontend creates a VideoFrame / MediaStreamTrack from the data
    }
    Ok(())
}
```

#### Linux Implementation (Rust-side)
Use XDG Desktop Portal + PipeWire:
```rust
#[tauri::command]
async fn start_native_screen_capture() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        // Call org.freedesktop.portal.ScreenCast via D-Bus
        // This triggers the native GNOME/KDE screen picker dialog
        // Receive PipeWire fd → capture frames → pipe to frontend
    }
    Ok(())
}
```

#### Alternative: Browser-Based Fallback
For web guests joining from a regular browser (Chrome/Edge), `getDisplayMedia()` works everywhere. The platform-specific handling is only needed for the Tauri desktop app's embedded webview.

---

## 6. Tailscale Integration Patterns

### Detecting Tailscale IP
```rust
#[tauri::command]
fn get_tailscale_info() -> TailscaleInfo {
    // Strategy 1: CLI query
    let output = Command::new("tailscale").args(&["ip", "-4"]).output();
    if let Ok(out) = output {
        if out.status.success() {
            let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !ip.is_empty() {
                return TailscaleInfo { ip, status: "Connected".into() };
            }
        }
    }

    // Strategy 2: Check network interfaces for 100.x.x.x
    // (cross-platform fallback using pnet or if_addrs crate)

    // Strategy 3: Fallback
    TailscaleInfo { ip: "127.0.0.1".into(), status: "Not Detected".into() }
}
```

### Tailscale Status Check
```rust
#[tauri::command]
fn get_tailscale_status() -> serde_json::Value {
    let output = Command::new("tailscale").args(&["status", "--json"]).output();
    match output {
        Ok(out) if out.status.success() => {
            serde_json::from_slice(&out.stdout).unwrap_or(serde_json::json!({"error": "parse failed"}))
        }
        _ => serde_json::json!({"error": "tailscale not running"})
    }
}
```

### Generating Invite Links
```typescript
function generateInviteLink(tailscaleIp: string, roomId: string): string {
  // Use HTTPS port (3443) for secure context (required for getUserMedia)
  return `https://${tailscaleIp}:3443/#room=${roomId}`;
}
```

### Tailscale Free Plan Limits (as of 2026)
- **100 devices** per tailnet
- **3 users** (human operators)
- **Unlimited data transfer** between devices
- **MagicDNS** enabled (can use hostnames instead of IPs)
- All connections are **WireGuard encrypted** end-to-end
- **Direct P2P** when possible, **DERP relay** through Tailscale's infra when direct path isn't available (e.g., behind very strict corporate firewalls)

---

## 7. Web Audio API — Noise Gate & HPF

### Proper Outgoing Audio Pipeline
The current codebase only **reads** audio levels but never **processes** the outgoing track. Here's the correct pipeline:

```
Raw Mic Track
    │
    ▼
MediaStreamSource
    │
    ▼
BiquadFilterNode (highpass, 80Hz)  ← Removes low-freq hum, rumble
    │
    ▼
DynamicsCompressorNode             ← Acts as noise gate
    │                                 threshold: configurable (-45dB default)
    │                                 knee: 0 (hard gate)
    │                                 ratio: 20 (aggressive compression below threshold)
    │                                 attack: 0.003s
    │                                 release: 0.25s
    │
    ▼
AnalyserNode                       ← Reads levels for UI volume meters
    │
    ▼
MediaStreamDestination             ← Produces a NEW processed MediaStream
    │
    ▼
Replace raw mic track on all       ← This processed track goes to peers
RTCPeerConnection senders
```

### Code Pattern
```typescript
function createProcessedAudioStream(rawStream: MediaStream): MediaStream {
  const ctx = new AudioContext();
  const source = ctx.createMediaStreamSource(rawStream);

  // 80Hz High-Pass Filter
  const hpf = ctx.createBiquadFilter();
  hpf.type = 'highpass';
  hpf.frequency.value = 80;
  hpf.Q.value = 0.7;

  // Noise Gate (DynamicsCompressor with hard settings)
  const gate = ctx.createDynamicsCompressor();
  gate.threshold.value = -45; // Configurable via UI slider
  gate.knee.value = 0;
  gate.ratio.value = 20;
  gate.attack.value = 0.003;
  gate.release.value = 0.25;

  // Volume analyser (for UI meters)
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 64;

  // Output destination
  const destination = ctx.createMediaStreamDestination();

  // Wire the chain
  source.connect(hpf);
  hpf.connect(gate);
  gate.connect(analyser);
  gate.connect(destination);

  return destination.stream; // Use THIS stream's audio track for WebRTC
}
```

---

## 8. WebRTC Mesh Topology & Limits

### Full Mesh
Every peer connects to every other peer directly. Connection count = `n * (n-1) / 2`.

| Peers | Connections | Upstream Bandwidth (per peer) | CPU Load |
|-------|------------|-------------------------------|----------|
| 2 | 1 | 1 stream out | Low |
| 3 | 3 | 2 streams out | Low |
| 4 | 6 | 3 streams out | Medium |
| 5 | 10 | 4 streams out | Medium-High |
| 6 | 15 | 5 streams out | High |

### Recommended Constraints Per Track
```typescript
// Webcam — balance quality vs bandwidth
const webcamConstraints = {
  width: { ideal: 1280, max: 1920 },
  height: { ideal: 720, max: 1080 },
  frameRate: { ideal: 30, max: 45 }
};

// Screen share — maximize quality
const screenConstraints = {
  width: { ideal: 1920 },
  height: { ideal: 1080 },
  frameRate: { ideal: 60 },
  // @ts-ignore — Chrome-specific but useful
  cursor: 'always'
};
```

### Peer Connection Lifecycle
```
1. Peer A joins room → signaling server notifies existing peers
2. For each existing peer B:
   a. A creates RTCPeerConnection
   b. A adds local tracks (audio + video)
   c. A creates SDP offer → sends via signaling server → B
   d. B creates RTCPeerConnection, adds local tracks
   e. B sets remote description (A's offer)
   f. B creates SDP answer → sends via signaling server → A
   g. A sets remote description (B's answer)
   h. ICE candidates exchanged bidirectionally
   i. Connection established → remote tracks received
3. On disconnect: close RTCPeerConnection, remove peer card from UI
```

### ICE Restart Strategy
```typescript
peerConnection.addEventListener('iceconnectionstatechange', () => {
  if (peerConnection.iceConnectionState === 'failed') {
    // Attempt ICE restart
    peerConnection.restartIce();
    // Re-create and send offer with iceRestart flag
    const offer = await peerConnection.createOffer({ iceRestart: true });
    await peerConnection.setLocalDescription(offer);
    signaling.send('signal', { targetId: peerId, signal: { sdp: peerConnection.localDescription } });
  }
});
```

---

## 9. Build Dependencies Per Platform

### Linux (Zorin OS / Ubuntu 22.04+)
```bash
# System build dependencies
sudo apt update && sudo apt install -y \
  build-essential \
  curl wget file \
  libwebkit2gtk-4.1-dev \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev

# GStreamer (required for WebRTC in WebKitGTK)
sudo apt install -y \
  gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly \
  gstreamer1.0-libav

# PipeWire + Portal (for screen sharing on Linux)
sudo apt install -y \
  gstreamer1.0-pipewire \
  xdg-desktop-portal-gtk

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Node.js (via nvm or system package)
# Tauri CLI
cargo install tauri-cli
```

### macOS
```bash
# Xcode Command Line Tools
xcode-select --install

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add aarch64-apple-darwin  # Apple Silicon
rustup target add x86_64-apple-darwin   # Intel

# Node.js (via Homebrew or nvm)
brew install node

# Tauri CLI
cargo install tauri-cli
```

### Windows 11
```powershell
# Visual Studio Build Tools (C++ workload)
# Download from https://visualstudio.microsoft.com/visual-cpp-build-tools/
# Select "Desktop development with C++" workload

# Windows SDK (included with VS Build Tools)

# Rust
# Download rustup-init.exe from https://rustup.rs
rustup target add x86_64-pc-windows-msvc

# Node.js (via official installer or winget)
winget install OpenJS.NodeJS.LTS

# Tauri CLI
cargo install tauri-cli

# WebView2 Runtime (pre-installed on Windows 11, auto-installed by Tauri on Win10)
```

---

## 10. Known Bugs & Workarounds

### Linux (Chrome/Chromium): Screen-share audio only works when sharing a tab
**Applies to**: PWA in Chrome/Chromium on Linux. macOS/Windows Chrome are unaffected (system audio works for screen/window/tab); Firefox on Linux works via PipeWire.
**Cause**: Chromium on Linux does not implement system-audio capture for screen/window `getDisplayMedia` sources. The picker shows no "Share audio" checkbox for screen/window picks, and the returned stream has no audio track — even with `audio: true` requested. Receiver-side code cannot compensate: there is no track to relay.
**Workarounds** (no true fix exists — it's a browser limitation, not an app bug):
1. Share a **tab** instead of a screen/window — tab capture always carries audio.
2. Use **Firefox on Linux** — supports system audio via PipeWire (shows a "Share audio" checkbox for screen/window picks).
3. Power-user hack: create a PipeWire/PulseAudio null sink with a loopback of system audio, then select that virtual device as the ZanCord mic. Everyone hears system audio as your mic — mute the real mic; only sensible for music/video playback.
**In-app handling**: `MediaManager.startScreenShare()` requests `audio: true`; if no audio track arrives within 1.5 s it shows a platform-aware toast. `screenAudioSupport()` in `src/utils/browser.ts` detects `linux-chromium` UAs and tells the user to share a tab (or keep using the mic). The mic stays live during screen share, so narration always works.

### Linux: `NotAllowedError` on `enumerateDevices()`
**Cause**: PipeWire or V4L2 permissions are restricted, or WebKitGTK returns empty device labels.
**Workaround**: Call `getUserMedia()` first (which triggers the permission grant), then call `enumerateDevices()`. Device labels are only populated after an active media grant.

### Linux: Silent audio/video failure
**Cause**: Missing GStreamer plugins (VP8, Opus, H.264 codecs).
**Workaround**: Install all GStreamer plugin packages listed above. Check with:
```bash
gst-inspect-1.0 | grep -E "opus|vp8|x264|webrtc"
```

### macOS: Permission denied after user clicks "Don't Allow"
**Cause**: macOS TCC caches the denial. WKWebView cannot re-trigger the system prompt.
**Workaround**: Detect the denied state and show a UI message instructing the user to go to `System Settings > Privacy & Security > Camera / Microphone` and re-enable for ZanCord.

### macOS: `getDisplayMedia` throws `NotAllowedError`
**Cause**: WKWebView doesn't support the screen picker modal.
**Workaround**: Use native Rust screen capture (see section 5). Or instruct users to use screen share only from the web browser client (Chrome) if native capture isn't implemented yet.

### Windows: Cached permission denial in WebView2
**Cause**: WebView2 caches permission state in `%LOCALAPPDATA%\<app>\EBWebView\Default\`.
**Workaround**: Use WebView2's `PermissionRequested` event handler to programmatically grant permissions:
```rust
// In Tauri/Wry, handle permission events
// WebView2 allows ALLOW/DENY/DEFAULT responses to permission requests
```

### All Platforms: ICE fails even on same Tailnet
**Cause**: WebRTC ICE agent may not enumerate the Tailscale virtual network interface.
**Workaround**: Pass Tailscale IPs through the signaling server. Each peer announces its Tailscale IP when joining, and other peers can add it as a remote candidate hint.

### All Platforms: `srcObject` assignment fails with CSP error
**Cause**: Content Security Policy blocks `blob:` origins.
**Fix**: Ensure CSP in `tauri.conf.json` includes `blob:`:
```
default-src 'self' 'unsafe-inline' 'unsafe-eval' wss: https: http: data: blob:;
```

---

## 11. Tauri v2 Configuration Reference

### `tauri.conf.json` — Key Sections
```jsonc
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "ZanCord",
  "version": "2.0.0",
  "identifier": "com.zancord.desktop",
  "build": {
    // Points to Vite's dev server during development
    "devUrl": "http://localhost:5173",
    // Points to Vite's build output for production
    "frontendDist": "../dist"
  },
  "app": {
    "withGlobalTauri": true,
    "windows": [{
      "title": "ZanCord | P2P Stream & Voice",
      "width": 1280,
      "height": 800,
      "resizable": true,
      "fullscreen": false,
      "decorations": true
    }],
    "security": {
      "csp": "default-src 'self' 'unsafe-inline' 'unsafe-eval' wss: https: http: data: blob:; media-src 'self' blob:; connect-src 'self' wss: ws: https: http:;"
    }
  },
  "bundle": {
    "active": true,
    "targets": ["deb", "appimage", "dmg", "app", "nsis"],
    "resources": ["../server.js", "../package.json"],
    "macOS": {
      "entitlements": "Entitlements.plist",
      "signingIdentity": "-",
      "minimumSystemVersion": "12.3"
    },
    "linux": {
      "depends": [
        "libwebkit2gtk-4.1-0",
        "gstreamer1.0-plugins-good",
        "gstreamer1.0-plugins-bad"
      ]
    }
  }
}
```

### `src-tauri/capabilities/default.json`
```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "main-capability",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "shell:allow-open",
    "process:default"
  ]
}
```

### Vite Integration
In `vite.config.ts`:
```typescript
import { defineConfig } from 'vite';

export default defineConfig({
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: ['es2021', 'chrome100', 'safari14'],
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG
  }
});
```
