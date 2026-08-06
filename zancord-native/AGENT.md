# Zancord Native — Agent Instructions

> **Read this file before making ANY changes to the zancord-native workspace.**
> Single source of truth for architectural decisions, constraints, and conventions.
> This is a **new native (non-PWA) implementation**; it supersedes the PWA code in
> the parent directory. The PWA remains in place but is not part of this workspace.

---

## Project Identity

**Zancord Native** is a zero-telemetry, Tailscale-native, peer-to-peer 1080p video
call, voice chat, and screen sharing application implemented in **Rust** with a
**Slint** UI. Full-mesh P2P over Tailscale — no STUN/TURN, no browser.

---

## Locked-In Architecture Decisions

These decisions are FINAL. Do not deviate without explicit user approval.

### Tech Stack
| Layer | Technology | Notes |
|-------|-----------|-------|
| **Language** | Rust (stable, edition 2021) | `rust-toolchain.toml` pins stable |
| **UI** | Slint (`.slint` declarative UI + Rust callbacks) | NO other GUI framework |
| **Signaling** | Axum (HTTP) + WebSocket | `GET /ws/:room_id`, in-memory rooms, zero database |
| **Networking** | Tailscale Mesh (WireGuard) | NO STUN/TURN. Peers connect via Tailscale `100.x.x.x` IPs |
| **Transport** | webrtc-rs (`webrtc` crate) | Full mesh, max 6 peers (5 remote) |
| **Audio I/O** | cpal + `rtrb` (lock-free SPSC) | Real-time callback rules strictly enforced |
| **Audio Codec** | Opus (48kHz mono, 20ms frames, FEC) | `opus` crate |
| **Audio Processing** | Biquad HPF (80Hz) + noise gate | Ported from PWA Web Audio pipeline |
| **Video Codecs** | H.264 (openh264) | Encoder/decoder abstraction in `zancord-video`. VP8 was planned but the vpx crate's interfaces are unusable — H.264-only is fine for a self-hosted mesh (both ends are this app) |
| **Screen Capture** | macOS ScreenCaptureKit / Linux PipeWire+XDG portal / Windows Desktop Duplication API (displays only, native resolution) | Per-platform `#[cfg]` modules; Windows has no system audio yet |
| **Camera** | nokhwa (or eye) | Cross-platform webcam capture |
| **Logging** | `tracing` + `tracing-subscriber` | Structured spans, env-filter |

### Network Architecture
- **Tailscale-native**: all peers assumed on the same Tailnet. No public internet
  connectivity is needed or supported.
- **No STUN/TURN**: WebRTC ICE config must be `ice_servers: []`. Peers connect
  directly via Tailscale IPs.
- **Signaling server** runs on one peer's machine, other peers connect via
  `https://<tailscale-ip>:3443` (TLS) or `http://<tailscale-ip>:3000` (plain).
- Full mesh topology, target max **6 simultaneous peers** (5 remote connections).

### Workspace Structure
```
zancord-native/
├── Cargo.toml                          # Workspace manifest
├── rust-toolchain.toml                 # Pinned stable toolchain
├── .cargo/config.toml                  # Linker config, platform flags
├── AGENT.md                            # This file
├── README.md
│
├── crates/
│   ├── zancord-protocol/               # Shared types & message definitions
│   ├── zancord-signaling-server/       # Axum + WS signaling server binary
│   ├── zancord-signaling-client/       # WS client library (tokio-tungstenite)
│   ├── zancord-audio/                  # Mic I/O, audio processing, Opus codec
│   ├── zancord-capture/                # Platform-specific screen + system audio
│   ├── zancord-video/                  # Video encode/decode (H.264, VP8)
│   ├── zancord-transport/              # WebRTC peer mesh, tracks, negotiation
│   └── zancord-app/                    # Main binary + Slint UI
│       └── ui/                         # .slint files
│
└── resources/
    ├── icons/                          # App icons (macOS .icns, Linux .png)
    └── macos/
        └── Info.plist                  # macOS entitlements & TCC descriptions
```

### Crate Dependency Rules
- `zancord-protocol` is the ONLY crate every other crate may depend on (besides
  workspace deps).
- `zancord-app` is the only crate that depends on ALL others (the composition root).
- No crate may depend on `zancord-app`.
- Transport ↔ audio/video connections are **runtime channels** (mpsc/rtrb), not
  compile-time dependencies.

### Threading Model
```
┌──────────────────────────────────────────────────────────┐
│  Main Thread (Slint Event Loop)                          │
│  - UI rendering / input / video frame display            │
└──────────────────────┬───────────────────────────────────┘
                       │ invoke_from_event_loop()
┌──────────────────────┴───────────────────────────────────┐
│  Tokio Runtime (multi-threaded)                          │
│  - WebRTC peer connections, signaling WS, RTP loops      │
│  - Video encode/decode tasks, capture event routing      │
└──────────────────────┬───────────────────────────────────┘
                       │ rtrb (lock-free SPSC)
┌──────────────────────┴───────────────────────────────────┐
│  Dedicated Audio Thread (std::thread, real-time priority)│
│  - cpal callbacks ↔ rtrb; processing at 20ms ticks       │
│  - Opus encode/decode + mixer                            │
└──────────────────────────────────────────────────────────┘
```

---

## Critical Rules

### Audio (cpal) — REAL-TIME SAFETY
The cpal audio callback runs on a **real-time OS thread**. Inside the callback you
MUST NOT:
- Allocate heap memory (`Vec::new()`, `Box::new()`, `String`)
- Acquire any `Mutex` or `RwLock`
- Use `println!` or any I/O
- Call `.await` or block

Only lock-free `rtrb::Producer::push()` / `rtrb::Consumer::pop()` inside callbacks.
All processing happens on a dedicated worker thread.

### WebRTC Negotiation
- Perfect negotiation ONLY (polite = lexicographically smaller peer id).
- Adding/removing tracks triggers `on_negotiation_needed` — MUST flow through the
  negotiation manager. NEVER create raw offers outside it (glare deadlocks).
- ICE config is `ice_servers: []` — no STUN/TURN, ever.

### Signaling Server
- Room fan-out: `tokio::sync::broadcast`, messages must carry the sender's peer ID
  so clients can filter their own broadcasts. Do NOT use `mpsc` for room broadcast.
- Room capacity: 6 (reject with `RoomFull`).
- Rate limits per peer: signal 30/s, chat 5/s, state 10/s, join 3/10s.

### Platform
- All platform-specific code gated with `#[cfg(target_os = "...")]`.
- macOS: screen capture requires `NSScreenCaptureUsageDescription` in Info.plist
  and a signed binary (ad-hoc `codesign --force --sign -` for dev).
- No hardcoded file paths anywhere.

---

## Coding Conventions

- Strict Rust: no `unsafe` unless absolutely required and documented.
- Errors: `anyhow::Result` for app-level, `thiserror` for library error enums.
- Logging: `tracing` everywhere, `[ZANCORD <crate>]` targets. Levels: ERROR failures,
  WARN degradations, INFO lifecycle, DEBUG signaling messages, TRACE RTP packets.
- `#[deny(clippy::all)]` at crate root of every library crate.
- Serde: all protocol types derive `Serialize, Deserialize`; messages are
  adjacently tagged `#[serde(tag = "type", content = "payload")]`.
- Comments only where non-obvious intent/tradeoffs exist — no restating code.

---

## Testing

- Unit + integration tests per crate (`cargo test --workspace`).
- Bug fixes land with a failing test first (TDD).
- No feature is complete until `cargo check --workspace` and `cargo test --workspace`
  pass and `cargo clippy --workspace -- -D warnings` is clean.

---

## Build & Run

```bash
cargo check --workspace
cargo run -p zancord-signaling-server
cargo run -p zancord-app --release
cargo test --workspace
cargo clippy --workspace -- -D warnings
codesign --force --sign - target/release/zancord-app   # macOS dev signing
```
