# ZanCord 🚀

**ZanCord** is a zero-telemetry, Tailscale-native, peer-to-peer 1080p video call, voice chat, and screen sharing **PWA** (progressive web app). No installs, no accounts — open a link, allow camera/mic, done. It runs in any modern browser on macOS, Windows, Linux, and mobile.

---

## ✨ Features
- **Zero Telemetry & 100% P2P**: Direct peer-to-peer encrypted audio/video over WebRTC — signaling server never touches media.
- **Tailscale Mesh**: All peers connect via Tailscale `100.x` IPs. No STUN/TURN, no public internet required.
- **Installable PWA**: Icons, manifest, and service worker included — "Install" from the browser for an app-like experience.
- **Noise Gate & Low-Cut Filter**: 80Hz high-pass + client-side noise gate (Web Audio API).
- **Chat**: In-room text chat with unread badges.
- **Screen Sharing**: Native `getDisplayMedia` on every platform (browser picker), with a resolution/FPS picker after you choose your source.
  > ⚠️ **Screen audio depends on browser + OS**: Chrome/Edge on macOS & Windows can carry system audio from a screen/window share (tick the picker checkbox). On **Linux**, only **tab** shares carry audio by default — unless Chrome is launched with `--enable-features=PulseaudioLoopbackForScreenShare`, which adds an "Also share system audio" checkbox for screen/window shares. Firefox only carries audio from tab shares (all OSes); Safari can't capture screen audio at all. Fallback that works everywhere: pick a "Monitor of …" audio source as your microphone in Settings. The app warns you inline when a capture has no audio.
- **Perfect WebRTC negotiation**: any peer can start sharing — glare-safe.

---

## 🚀 How to Run

### 1️⃣ Install Dependencies
```bash
npm install
```

### 2️⃣ Build the PWA
```bash
npm run build        # tsc + vite → dist/
```

### 3️⃣ Run Signaling Server (serves the PWA too)
```bash
npm run server
```
- App: `http://localhost:3000` (and `https://localhost:3443` if `key.pem`/`cert.pem` exist)
- Signaling: same origin, `/socket.io` — no separate client URL needed.

### 4️⃣ Invite a Friend
1. Open the app — installed PWAs automatically meet in the **shared room** (`zancord-room`): if you and a friend both have the app open, you're connected. No link needed.
2. For guests or private rooms, click **Copy Link** and send `https://<YOUR_TAILSCALE_IP>:3443/#room=<room>` — the `#room=` part overrides the shared default.
3. Guest opens it in Chrome/Edge/Firefox/Safari and clicks **Allow** for camera & mic.

> 💡 **Production tip:** use `tailscale serve` for auto-renewed HTTPS certificates and no self-signed warnings:
> ```bash
> tailscale serve --bg --https=443 http://localhost:3000
> ```

---

## 🧪 Testing (TDD)
```bash
npm test            # vitest — 108 tests across server, WebRTC negotiation, UI, chat, rooms, audio, PWA manifest
npm run test:watch
```
Test-first development: every bug fix lands with a failing test first. See `AUDIT.md` for the full audit and the test manifest.

## 🧠 Agent Skills (Zed)

A focused skill stack (sourced from [Agentic Awesome Skills](https://github.com/sickn33/agentic-awesome-skills), converted for the Zed agent) is installed globally in `~/.agents/skills/` — the Zed agent loads them on demand via its `skill` tool:

| Skill | Use it for |
|---|---|
| `brainstorming` | Planning features before coding |
| `systematic-debugging` / `debugging-toolkit` | Structured bug hunting |
| `tdd` | Writing tests first (project rule) |
| `typescript-expert` | TS strict-mode work |
| `nodejs-best-practices` / `backend-dev-guidelines` | Server-side (server.js) |
| `frontend-dev-guidelines` / `frontend-architecture` | UI work |
| `code-reviewer` | Reviewing changes |
| `git-workflow-and-versioning` | Commits/branches |
| `api-security-best-practices` | Signaling/API hardening |
| `observability-and-instrumentation` | Logging/monitoring |

**Plus 30 UX/UI skills** (same source, also global): `design-ux`, `design-thinking`, `design-philosophy`, `design-system`, `design-taste-frontend`, `design-spells`, `design-spatial`, `design-orchestration`, `design-it`, `design-md`, `deterministic-design`, `frontend-design`, `frontend-ui-engineering`, `mobile-design`, `accessibility-compliance-accessibility-audit`, `fixing-accessibility`, `ckw-design`, `emil-design-eng`, `high-end-visual-design`, `iconsax-library`, `canvas-design`, `antigravity-design-expert`, `ux-audit`, `uxui-principles`, `ui-ux-designer`, `ui-ux-pro-max`, `baseline-ui`, `theme-factory`, `fixing-motion-performance`, `review-animations`.

Ask the agent to use one (e.g. "Use systematic-debugging to find why…").

---

## 🗂 Project Layout
```
server.js                # Express + Socket.io signaling server (also serves dist/)
index.html               # PWA entry
src/
├── main.ts              # Orchestrator
├── core/                # EventBus, RoomManager, PeerManager, MediaManager, AudioProcessor
├── ui/                  # UIRenderer, ChatManager, ToastManager
├── utils/room.ts        # Room id parsing/generation
├── types/               # Shared TypeScript interfaces
└── styles/style.css     # Complete stylesheet
public/                  # PWA shell: manifest.json, sw.js, generated icons
scripts/generate-icons.mjs  # Zero-dependency PWA icon generator (npm run icons)
tests/                   # Vitest suites (see AUDIT.md test manifest)
```

---

## 🔐 Security Notes
- The server serves **only** `dist/` — never source, configs, or TLS keys.
- CSP + nosniff + frame-ancestors headers set on every response.
- Rate limits: signal 30/s, chat 5/s, state 10/s, joins 3/10s per socket.
- Rooms are invite-only by link; a unique room id is generated per session.
