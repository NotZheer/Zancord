# ZanCord — Full Audit (PWA Migration Edition)

> Date: 2026-08-01 · Author: Orchestrator agent
> Scope: UX/UI interactions, backend problems, PWA-migration readiness.
> Status legend: ✅ Fixed · 🟡 Deferred · ❌ Open
>
> **Performance & efficiency audit lives in [`PERF-AUDIT.md`](PERF-AUDIT.md)** (2026-08-03):
> screen-capture resolution caps, metering-loop throttling, bounded log buffer, mesh transport policy.

---

## 0. Context

ZanCord is pivoting from a **Tauri v2 desktop shell** to a **pure PWA** (browser-only, served over HTTPS from the signaling server). All Tauri-specific code is slated for removal. This audit catalogues the problems found before the migration, and tracks which were fixed (test-first) as part of this pass.

---

## 1. 🔴 Critical

### C1 — Signaling server serves the entire project directory (private keys exposed)
`server.js` does `app.use(express.static(__dirname))` — the static root is the whole repo. Anyone on the network can fetch:

- `https://<ip>:3443/key.pem` and `cert.pem` → **the TLS private keys**
- `server.js`, `package.json`, `package-lock.json`, `src/**`, `node_modules/**`

`key.pem`/`cert.pem` are also **not in `.gitignore`**.
- ✅ Fix (TDD): static root scoped to `dist/` only (`dotfiles: 'deny'`); certs added to `.gitignore`; regression tests assert `GET /key.pem`, `/cert.pem`, `/package.json` → 404.
- Test: `tests/server.test.ts — "static scoping"`

### C2 — The invite link serves a broken app
The root `index.html` (Vite dev entry) references `/src/main.ts` as a module. Express serves the raw `.ts` file (MIME `video/mp2t`) → module refused → the app never boots for guests on `https://<tailscale-ip>:3443/#room=...`. The built `dist/` exists but was never served. The only "working" deployment today is the Vite dev server.
- ✅ Fix (TDD): server serves `dist/` with `GET /` → `dist/index.html`; test asserts the served HTML is the built shell (`id="app"`), not the dev entry.
- Test: `tests/server.test.ts — "serves built app"`

### C3 — Screen share only works for one peer per pair (WebRTC negotiation)
`PeerManager.onnegotiationneeded` only creates offers when `isInitiator` is true, and the initiator is always the **later joiner** of each pair. When a non-initiator calls `addScreenTrack()` → `pc.addTrack()` → negotiation-needed fires → **no offer is created** → the remote peer never receives the screen track.
- ✅ Fix (TDD): perfect-negotiation pattern (both sides may offer; polite/impolite glare handling keyed off socket-id comparison). Regression tests: non-initiator screen share emits an offer; simultaneous double-offer converges.
- Test: `tests/peerManager.test.ts — "screen share negotiation" / "glare convergence"`

---

## 2. 🟠 High — Backend (`server.js`)

| # | Issue | Status | Test |
|---|-------|--------|------|
| B1 | No input validation: unbounded usernames (emoji spam observed in `signaling.log`), unvalidated `roomId` | ✅ `normalizeUsername` (trim, strip control chars, cap 24) + `isValidRoomId` (1–64, `[A-Za-z0-9_-]`); join aborted on invalid room | `server.test.ts — validation` |
| B2 | Only `signal` (30/s) and `chat` (5/s) rate-limited; `join-room` and `peer-state-changed` are free → socket-flood DoS, no socket cap | ✅ join limited to 3/10s per socket, state-change 10/s; rate limiter extracted as pure, unit-tested function | `server.test.ts — rate limits` |
| B3 | Signal relay trusts any `targetId` (no room scoping) | 🟡 Deferred: requires room→socket membership map on relay; low practical risk on trusted tailnet, but should land with auth work | — |
| B4 | No auth + default room `zancord-room` when URL has no hash → everyone lands in the same room; room names guessable, never expire | ✅ Reworked: **shared default room by design** — the server is only reachable on the private tailnet, so a common meeting room is what makes installed PWAs connect instantly; `#room=` links still override for guests/private rooms | `tests/room.test.ts` |
| B5 | Reconnect changes socket identity: re-join as "new user", stale `PeerManager.peers` entries, card churn | 🟡 Deferred: mostly self-healing (old socket's `user-left` closes stale conns); needs a stable client UUID + renegotiation to be fully correct | — |
| B6 | No security headers (CSP, nosniff, referrer, frame options) — Tauri CSP dies with the shell | ✅ Header middleware on the server; CSP tuned for CDN scripts, Google Fonts, `blob:` media, `ws/wss` signaling | `server.test.ts — headers` |
| B7 | HTTPS optional; self-signed certs give guests scary warnings | 🟡 Partially: server keeps HTTPS-on-3443. Recommend **Tailscale Serve** for auto-renewed certs + no key management (follow-up, ops task) | — |

---

## 3. 🟠 High — UX/UI Broken Interactions

| # | Issue | Status |
|---|-------|--------|
| U1 | **Chat is dead code**: `ChatManager` never instantiated, no drawer in `index.html` (CSS + server support exist) | ✅ Wired: drawer markup added, `ChatManager` instantiated, unread badge, drawer toggle |
| U2 | **Deafen doesn't deafen newcomers**: mutes only currently-rendered videos | ✅ `UIRenderer.setDeafened()` — deafen state is applied to every card created/stream-attached while deafened |
| U3 | **Remote state invisible**: cards have no name/mute/connection UI; `setConnectionQuality` is a no-op; remote audio-level never measured | ✅ Name label + mic-mute badge + connection-state class on cards; **remote metering ✅ `RemoteAudioMeter`** — per-stream silent Web Audio taps feed the speaking ring on the right tile (cam AND screen share), so a silent capture is now visually distinguishable from a broken render |
| U4 | **Display-name change does nothing**: `updatePeerState` ignores `username` | ✅ Name label renders/updates (escaped) |
| U5 | **Camera-strip drag is mouse-only** (`mousedown/mousemove`), broken on touch | ✅ Converted to Pointer Events + `setPointerCapture` |
| U6 | **No connectivity feedback**: `SOCKET_DISCONNECTED` unhandled | ✅ Toast on disconnect; `SOCKET_CONNECTED` feeds `PeerManager.setLocalId()` |
| U7 | **Remote-audio autoplay risk**: remote `<video>` unmuted → Chrome may block `play()` with sound | 🟡 Deferred: standard fix is start-muted + unmute on first pointer interaction; needs UX decision |
| U8 | **AudioContext autoplay risk**: pipeline created without gesture, no `resume()` | 🟡 Deferred: add explicit resume-on-first-interaction |
| U9 | **`leaveCall()` reloads window** after 1s; state cleared but hash kept | 🟡 Deferred: acceptable for now |
| U10 | **`peer-screen-stream-removed` has no listener** — remote screen-card cleanup races | ✅ Listener added |

---

## 4. 🟡 Medium — Correctness & Polish

| # | Issue | Status |
|---|-------|--------|
| M1 | `handleSignal` names unknown peers `"Peer"` (race) | 🟡 Deferred — harmless cosmetic |
| M2 | `switchMicrophone` churns AudioContexts | 🟡 Deferred |
| M3 | "720P @ 30 FPS" in settings vs 1080p branding; camera constraints `ideal: 1280×720/30` | 🟡 Deferred — decide the story |
| M4 | Invite link hardcodes `:3443`; clipboard has no fallback | 🟡 Deferred |
| M5 | Boot script `kill -9` on ports 3000/3443/5173 | ✅ Kept but reworked to build + serve `dist/` (PWA production path) |
| M6 | `room-full` counts sockets not humans | 🟡 Deferred |
| M7 | Legacy `public/` client (v1): Google STUN servers (violates Tailscale-only), incompatible events (`user-state-change`), hardcoded IP | ✅ Deleted; single-client world |
| M8 | Screen share `audio: true` without echoCancellation | ✅ Constraints added (`echoCancellation`, `noiseSuppression`) |
| M9 | `navigator.userAgent.includes('Windows')` brittle | ✅ Removed with Tauri branch |

---

## 5. PWA-Specific Gaps

| # | Gap | Status |
|---|-----|--------|
| P1 | **Not installable**: manifest references `/icon.png` which doesn't exist | ✅ Icons generated (192/512/maskable/apple-touch) via `scripts/generate-icons.mjs`; manifest updated; test asserts icon files exist |
| P2 | Service worker network-first, no precache | ✅ Precache of shell assets + stale-while-revalidate for same-origin |
| P3 | HTTPS gate + self-signed cert warnings for guests | 🟡 Recommend Tailscale Serve (ops follow-up) |
| P4 | Background tab suspends AudioContext → dead mic | ✅ Wake Lock requested on first interaction + re-acquired on visibility (feature-detected) |
| P5 | Mobile: no facingMode, mesh heavy on phones | 🟡 Deferred |
| P6 | Touch support for camera strip | ✅ Pointer Events (same as U5) |

---

## 6. Tauri Removal — Complete Checklist

| Artifact | Status |
|----------|--------|
| `src-tauri/` (main.rs, Cargo.toml, tauri.conf.json, capabilities/, plists, build.rs, icons/) | ✅ Deleted |
| `@tauri-apps/api`, `@tauri-apps/cli`, `"tauri"` script in `package.json` | ✅ Removed |
| `main.ts` `detectTailscaleIP()` Tauri branch | ✅ Removed (hostname fallback only) |
| `RoomManager.connect()` `__TAURI__` branch | ✅ Removed (always same-origin) |
| `MediaManager.startScreenShare()` Tauri/Windows branch | ✅ Removed (always `getDisplayMedia`) |
| `vite.config.ts` `TAURI_DEBUG`/`TAURI_` envPrefix | ✅ Removed |
| Tauri CSP in `tauri.conf.json` | ✅ Replaced by server CSP headers |
| `start_zancord_on_boot.sh` dev-server-as-production | ✅ Reworked: build + `npm run server` |
| Docs (`AGENT.md`, `KNOWLEDGE.md`, `README.md`) | ✅ Amended/rewritten for PWA |

---

## 7. Test Manifest (TDD)

| Test file | Covers |
|-----------|--------|
| `tests/server.test.ts` | C1 static scoping, C2 built-app serving, B1 validation, B2 rate limits, B6 headers, room lifecycle, chat truncation/limits |
| `tests/peerManager.test.ts` | C3 screen-share negotiation, glare convergence, ICE queue, offer/answer flow |
| `tests/uiRenderer.test.ts` | U2 deafen persistence, U3 remote state badges, U4 name updates, audio-level glow |
| `tests/chatManager.test.ts` | U1 send/receive, XSS escaping, unread badge |
| `tests/room.test.ts` | B4 unique room id generation/parsing |
| `tests/manifest.test.ts` | P1 PWA manifest icon integrity |

Run: `npm test` (vitest). Build: `npm run build` → `dist/` served by `npm run server`.
