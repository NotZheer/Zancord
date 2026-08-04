# ZanCord — Performance & Efficiency Audit

> Date: 2026-08-03 · Scope: client rendering, media pipeline, WebRTC mesh, memory, server
> Status legend: ✅ Fixed (this pass) · 🟡 Deferred / recommendation

---

## 1. 🔴 High — Media & Encoding

### P1 — Screen capture runs at unbounded resolution (4K encode risk)
`MediaManager.startScreenShare()` requests `width: { ideal: 1920 }, height: { ideal: 1080 }` — **`ideal` is not a cap**. On a 4K/5K display Chrome captures at the display's native resolution and the VP8/H.264 encoder eats 4–6× the CPU of 1080p, then re-encodes once **per peer** (full mesh). This is the single biggest CPU/bandwidth cost in the app.
- ✅ Fix: **screen-share quality picker** — after the browser picker closes, a modal offers Resolution (Source native / 1080p / 720p / 540p / 360p) and FPS (60/30/15/10); the choice is applied via `track.applyConstraints()` with `max` caps and persisted (`zancord_share_resolution`, `zancord_share_fps`). Default 1080p30.
- Test: `tests/mediaManager.test.ts — "screen share quality constraints"` / `tests/shareQualityModal.test.ts`

### P2 — Screen share encoded once per peer with no bitrate budget
Each RTCPeerConnection gets its own sender for the screen track, so a 1080p60 share is encoded N times (N = peers − 1). Mesh topology makes this unavoidable, but the cost is unbounded: no `maxBitrate` on screen senders.
- 🟡 Recommendation: when the quality picker applies constraints, also clamp screen-sender bitrate via `RTCRtpSender.setParameters()` (`encodings[0].maxBitrate` ≈ 2500–4000 kbps at 1080p, 1500–2500 at 720p) and set `degradationPreference: 'maintain-framerate'`. Needs a renegotiation-safe helper; deferred to keep this pass surgical.

### P3 — Remote video is decoded at full sender resolution even when tiled small
The gallery renders 6 tiles but each remote stream decodes at the sender's captured resolution. Inherent to WebRTC without an SFU; the quality picker (P1) reduces the *sender's* cost but receivers still decode whatever arrives.
- 🟡 Recommendation (long-term): switch to an SFU (e.g. `mediasoup`/`ion-sfu`) on the Tailscale host, or accept the cost at ≤6 peers. Documented, not changed.

---

## 2. 🟠 Medium — Client CPU / Memory

### P4 — Debug log interceptor grows without bound
`main.ts` pushes **every** `console.log/warn/error` into `window.__ZANCORD_LOGS__` for the whole session and never trims. A long call (hours) with chat/signaling churn leaks a few MB and grows forever.
- ✅ Fix: capped ring buffer (last 500 entries, drops oldest) in `src/utils/logBuffer.ts`; `__ZANCORD_LOGS__` now exposes the capped buffer.
- Test: `tests/logBuffer.test.ts`

### P5 — Audio metering loop churns the UI at 60fps even when level is static
`AudioProcessor.startMetering()` runs a `requestAnimationFrame` loop that computes FFT data and **emits `AUDIO_LEVEL` + writes a CSS custom property + toggles classes on every frame**, even when the level hasn't changed (silence or constant speech) and even when the settings popover is closed. 60 style recalculations/sec on the card grid.
- ✅ Fix: emit only when the rounded level changes by ≥1 (pure helper `shouldEmitLevel`), and only call `setTargetAtTime` when the gate open/close target actually flips. Meter value still recomputed each frame (cheap) so `getAudioLevel()` stays current.
- Test: `tests/audioProcessor.test.ts — "level emission throttling"`

### P6 — PiP camera bubbles are positioned with layout properties during drag
`applyPipGeometry()` writes `--pip-left/--pip-top/--pip-width` consumed as `left/top` CSS — every `pointermove` during a drag forces a **layout pass** on the grid.
- ✅ Fix: position via `transform: translate3d(var(--pip-left), var(--pip-top), 0)` (compositor-only; `left/top` fixed at 0). `--pip-width` still drives `width` (layout) since resize inherently needs it. Drag math in `UIRenderer` is unchanged — `getBoundingClientRect()` already accounts for transforms.

### P7 — WebRTC config doesn't force a single transport per peer
Default `bundlePolicy: 'balanced'` may negotiate multiple transports when streams are added at different times (cam tracks first, screen track later) — more ICE candidates, more DTLS/SRTP overhead per peer in a 6-way mesh.
- ✅ Fix: `bundlePolicy: 'max-bundle'` — all m-lines for a peer share one transport. Standard mesh recommendation.
- Test: `tests/peerManager.test.ts — "bundle policy"`

### P8 — `switchMicrophone`/`switchCamera` churn AudioContexts and drop quality constraints
Known (AUDIT M2/M3): each mic switch rebuilds the whole Web Audio graph; camera switches use `deviceId` only, so the browser re-applies default (often native) resolution. Not perf-critical on the hot path; 🟡 deferred with the settings story.

---

## 3. 🟡 Low — Rendering & Layout

| # | Finding | Status |
|---|---------|--------|
| P9 | `updateGalleryLayout()` calls `getBoundingClientRect()` on every card add/remove and ResizeObserver tick — forced sync layout, but low frequency (user-driven events only). Fine at ≤6 cards. | ✅ acceptable |
| P10 | `setAudioLevel` writes `--audio-level` custom property + class toggle per event — now gated by P5 throttling. | ✅ acceptable |
| P11 | Remote `<video>` elements use `object-fit: cover`; no `width/height` attributes — browser downscales in compositor, no double decode. | ✅ acceptable |
| P12 | Font Awesome 6 full CDN stylesheet (~120 KB) + Google Fonts render-blocking — first paint cost; preconnect present. Could subset icons or self-host later. | 🟡 recommendation |
| P13 | Vite build: single JS bundle, no `manualChunks`, esbuild minify — fine for this app size; `sourcemap: false` correct for prod. | ✅ acceptable |
| P14 | `UIRenderer.setPeerStream` guards `srcObject` re-assignment and retries `play()` once audio lands — prevents decode restarts (AbortError churn). | ✅ good |

---

## 4. 🟠 Medium — Server

| # | Finding | Status |
|---|---------|--------|
| P15 | Static assets served without gzip/br compression (express.static, no `compression` middleware). JS/CSS compress ~70%; on Tailscale LAN latency is low but bytes still matter on mobile tails. | 🟡 recommendation: add `compression` middleware or reverse-proxy (Caddy/Tailscale Serve) with compression |
| P16 | `checkRateLimit` filters a per-socket timestamp array on every event — O(n) with n ≤ 30/s window; negligible. | ✅ acceptable |
| P17 | Socket.io `pingInterval: 5000` / `pingTimeout: 10000` — 0.4% overhead, good failover latency for tailnet. | ✅ acceptable |
| P18 | No room/peer count metrics — a `performance.now()`-based per-connection memory guard could be added later; not needed at ≤6 peers. | 🟡 recommendation |

---

## 5. Fix Manifest (TDD)

| Fix | Test |
|-----|------|
| P1 quality picker + constraints | `tests/mediaManager.test.ts`, `tests/shareQualityModal.test.ts` |
| P4 capped log buffer | `tests/logBuffer.test.ts` |
| P5 metering throttle | `tests/audioProcessor.test.ts` |
| P6 PiP compositor drag | existing `tests/uiRenderer.test.ts` PiP drag suite (unchanged assertions) |
| P7 max-bundle | `tests/peerManager.test.ts` |

Run: `npm test` (vitest). Build: `npm run build`.
