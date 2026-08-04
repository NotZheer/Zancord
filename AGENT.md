# ZanCord — Agent Instructions

> **Read this file before making ANY changes to the codebase.**
> This is the single source of truth for architectural decisions, constraints, and conventions.

---

## ⚡ PWA PIVOT — 2026-08-01 (SUPERSEDES TAURI SECTIONS BELOW)

ZanCord is now a **pure PWA** (browser-only). Approved changes:

- **Tauri is REMOVED.** No desktop shell, no `src-tauri/`, no `@tauri-apps/*`, no `__TAURI__` branches, no Rust.
- **Platform target**: modern browsers (Chrome/Edge/Firefox/Safari) on desktop + mobile, served over HTTPS from the signaling server (`npm run server`, serves `dist/`).
- **Screen sharing** is `getDisplayMedia()` everywhere (native browser picker). No Rust capture, no WebKitGTK.
- **Permissions** are handled by the browser (HTTPS + user gesture). No entitlements/plists.
- **Tailscale rule unchanged**: NO STUN/TURN; peers connect via Tailscale IPs; signaling is same-origin (`/socket.io`).
- **Testing is mandatory**: `npm test` (vitest, TDD — see `AUDIT.md`). Bug fixes land with a failing test first.
- **WebRTC negotiation**: perfect negotiation (any peer may offer; polite/impolite glare handling).
- **Rooms**: ONE shared default room (`zancord-room`) — installed PWAs connect automatically when both open the app. `#room=` links still override for guests/private rooms.
- **PWA assets**: `public/manifest.json`, `public/sw.js`, icons via `npm run icons` (`scripts/generate-icons.mjs`).

Everything below this line that contradicts the above is **historical** and must not be re-applied.

---

## Project Identity

**ZanCord** is a zero-telemetry, Tailscale-native, peer-to-peer 1080p video call, voice chat, and screen sharing **PWA** running in the browser on any OS (desktop and mobile).

---

## Locked-In Architecture Decisions

These decisions are FINAL. Do not deviate without explicit user approval.

### Tech Stack
| Layer | Technology | Notes |
|-------|-----------|-------|
| **Desktop Shell** | Tauri v2 (Rust) | Cross-platform: Linux + macOS + Windows |
| **Frontend Build** | Vite + TypeScript | NO React, NO framework. Vanilla TS + DOM manipulation |
| **Frontend Styling** | Vanilla CSS | NO Tailwind, NO CSS-in-JS |
| **Signaling Server** | Node.js + Express + Socket.io | Ephemeral in-memory rooms, zero database |
| **Networking** | Tailscale Mesh (WireGuard) | NO STUN/TURN servers. All peers connect via Tailscale `100.x.x.x` IPs |
| **Media** | WebRTC (browser API) | Full mesh topology, max ~6 peers |
| **Audio Processing** | Web Audio API | 80Hz HPF + noise gate on outgoing mic audio |
| **Fonts** | Google Fonts (Outfit + JetBrains Mono) | Loaded via CDN `<link>` tag |
| **Icons** | Font Awesome 6 | Loaded via CDN `<link>` tag |

### Network Architecture
- **Tailscale-native**: The entire app assumes all peers are on the same Tailnet. No public internet connectivity is needed or supported.
- **No STUN/TURN**: Tailscale handles NAT traversal via WireGuard tunnels. The WebRTC ICE configuration should contain NO STUN/TURN servers — peers connect directly via Tailscale IPs.
- **Signaling server** runs on one peer's machine. Other peers connect to it via `https://<tailscale-ip>:3443`.
- **Free plan scope**: Tailscale free tier = 100 devices, 3 users. Target max **6 simultaneous peers** in a room (full mesh).

### Frontend Architecture (Vite + TypeScript)
```
public/                     # Static assets (index.html loads from here via Vite)
src/
├── main.ts                 # Entry point — orchestrator
├── core/
│   ├── EventBus.ts         # Pub/sub decoupling layer
│   ├── RoomManager.ts      # Socket.io connection + room lifecycle
│   ├── PeerManager.ts      # N-peer RTCPeerConnection management
│   ├── MediaManager.ts     # Camera, mic, screen share acquisition
│   └── AudioProcessor.ts   # Web Audio noise gate + HPF pipeline
├── ui/
│   ├── UIRenderer.ts       # Dynamic peer card DOM creation/destruction
│   ├── ChatManager.ts      # Chat message rendering + form handling
│   └── ToastManager.ts     # Notification toasts
├── types/
│   └── index.ts            # Shared TypeScript interfaces
└── styles/
    └── style.css           # Complete stylesheet
```

### Desktop Architecture (Tauri v2 / Rust)
```
src-tauri/
├── src/
│   └── main.rs             # App entry, server lifecycle, Tailscale IPC, permissions
├── Cargo.toml              # Rust dependencies
├── tauri.conf.json          # Window config, CSP, bundling, resources
├── capabilities/
│   └── default.json         # Tauri v2 ACL permissions
├── Info.plist               # macOS camera/mic/screen usage descriptions
└── Entitlements.plist       # macOS hardened runtime entitlements
```

---

## Critical Cross-Platform Rules

### MUST follow these rules for every change:

1. **No hardcoded file paths.** Use `std::env::current_exe()`, Tauri's resource resolver, or `which` crate for binary discovery. NEVER hardcode `/home/username/...` or similar.

2. **Screen sharing requires platform-specific handling:**
   - **Windows**: `getDisplayMedia()` works natively in WebView2 (Chromium). Use the JS API directly.
   - **macOS**: `getDisplayMedia()` is broken in WKWebView. Must use native Rust screen capture via `ScreenCaptureKit` bindings, piped to WebRTC via a custom video track.
   - **Linux**: `getDisplayMedia()` is broken in WebKitGTK. Must use native Rust screen capture via PipeWire / `xcap`, piped to WebRTC via a custom video track.
   - **Strategy**: Use Tauri IPC commands to detect platform, then branch: JS-native on Windows, Rust-native on macOS/Linux.

3. **Camera/mic permissions require platform-specific setup:**
   - **Linux**: Must call `settings.set_enable_webrtc(true)` and `settings.set_enable_media_stream(true)` on the WebKitGTK WebView, AND auto-grant permission requests via `connect_permission_request`.
   - **macOS**: Must include `NSCameraUsageDescription`, `NSMicrophoneUsageDescription` in `Info.plist`. Must code-sign with camera + microphone entitlements.
   - **Windows**: Works automatically. WebView2 shows Chromium-style permission prompts.

4. **CSP in `tauri.conf.json` must permit media sources:**
   ```
   default-src 'self' 'unsafe-inline' 'unsafe-eval' wss: https: http: data: blob:; media-src 'self' blob:;
   ```

5. **Linux build dependencies** must be documented and declared in `.deb` package metadata:
   - `libwebkit2gtk-4.1-dev` (NOT 4.0)
   - `gstreamer1.0-plugins-base`, `gstreamer1.0-plugins-good`, `gstreamer1.0-plugins-bad`
   - `gstreamer1.0-pipewire`, `xdg-desktop-portal-gtk`

6. **Tauri v2 uses ACL capabilities** (NOT the old v1 allowlist). Permissions go in `src-tauri/capabilities/default.json`.

7. **All Tailscale IP detection** must gracefully fallback. Try `tailscale ip -4` first, then try reading from Tailscale socket/API, then fallback to `127.0.0.1` with a warning in the UI.

---

## Code Conventions

### TypeScript
- Use strict TypeScript (`"strict": true` in `tsconfig.json`)
- Define all data shapes as interfaces in `src/types/index.ts`
- Use `const enum` for event names to avoid runtime string typos
- No `any` types — use `unknown` + type guards if needed
- All Socket.io events must have typed payloads

### CSS
- Use CSS custom properties (variables) for all colors, spacing, fonts
- Dark theme by default (matte dark glassmorphism aesthetic)
- No `!important` unless overriding third-party styles
- Use `rem` units for font sizes, `px` for borders/shadows
- All interactive elements need hover/focus/active states
- All transitions use `cubic-bezier(0.16, 1, 0.3, 1)` for consistent spring feel

### Rust
- Use `#[cfg(target_os = "...")]` for all platform-specific code
- Use `anyhow::Result` for error handling
- Log with `println!("[ZANCORD ...]")` prefix for easy grep
- Store child process handles and kill on app exit
- Use `which` crate for finding `node` binary, never hardcode paths

### Git
- Commit messages: `feat:`, `fix:`, `refactor:`, `docs:`, `chore:` prefixes
- One logical change per commit

---

## Testing Checklist

Before considering any feature complete, verify:

- [ ] Works in Tauri desktop on Linux (Zorin OS / Ubuntu)
- [ ] Works in Tauri desktop on macOS
- [ ] Works in Tauri desktop on Windows 11
- [ ] Works when a web browser guest joins via Chrome/Edge on any OS
- [ ] Camera and microphone permissions are properly requested and granted
- [ ] Audio processing (noise gate + HPF) is applied to outgoing audio
- [ ] Peer cards dynamically appear/disappear when users join/leave
- [ ] Socket reconnection re-joins the room and re-establishes peer connections
- [ ] All dock buttons (mic, cam, screen, deafen, leave) are functional
- [ ] Chat messages send and receive in real-time
- [ ] Copy Link generates a valid Tailscale URL

---

## Files You Should NEVER Modify Without Reading This Document First

- `src-tauri/tauri.conf.json` — CSP, window config, bundling
- `src-tauri/src/main.rs` — Platform-specific permission hooks
- `src/core/PeerManager.ts` — WebRTC connection lifecycle
- `src/core/MediaManager.ts` — Media acquisition (platform-branching logic)
