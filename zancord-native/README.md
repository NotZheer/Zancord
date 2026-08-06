# Zancord Native

Zero-telemetry, Tailscale-native, peer-to-peer 1080p video call, voice chat, and
screen sharing — implemented in Rust with a Slint UI. Full-mesh P2P over Tailscale
(no STUN/TURN, no browser/PWA).

## Status

| Phase | Description | Status |
|-------|-------------|--------|
| 0 | Foundation & scaffold | ✅ |
| 1A/1B | Signaling server + client | ✅ |
| 1C | Audio pipeline | ✅ |
| 1D | WebRTC transport | ✅ |
| 2 | Voice call end-to-end (CLI harness) | ✅ loopback-verified |
| 3 | Capture + video | ✅ screen (SCK/PipeWire), camera (nokhwa), H.264/VP8 |
| 4 | Slint UI | ✅ |
| 5 | Full integration | 🟡 5.1 screen share E2E ✅; 5.2 camera E2E code-complete, awaiting on-device verification |
| 6 | Polish & parity | ⬜ |

## Workspace

```
crates/
├── zancord-protocol/          # Shared types & message definitions
├── zancord-signaling-server/  # Axum + WS signaling server binary
├── zancord-signaling-client/  # WS client library (tokio-tungstenite)
├── zancord-audio/             # Mic I/O, audio processing, Opus codec
├── zancord-capture/           # Platform-specific screen + system audio
├── zancord-video/             # Video encode/decode (H.264, VP8)
├── zancord-transport/         # WebRTC peer mesh, tracks, negotiation
└── zancord-app/               # Main binary + Slint UI
```

## Build & Run

```bash
cargo check --workspace
cargo run -p zancord-signaling-server   # signaling server on :3000 (TLS :3443 if certs present)
cargo run -p zancord-app --release      # the app (media-heavy, use release)
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

macOS screen capture requires signing: `codesign --force --sign - target/release/zancord-app`

## Media Profiles & Bitrate (Phase 6)

| Stream | Profile | Encoder target | Notes |
|--------|---------|----------------|-------|
| Camera | 720p30 | 2 Mbps | Software H.264 (openh264) — the encode budget for smooth 30 fps |
| Screen | 720p15 | 1.2 Mbps | Plus Opus screen-audio (macOS SCK / Linux PipeWire sink monitor) |

- **Why not 1080p by default?** The 1080p branding describes the architecture's
  ceiling, not the default profile: full-mesh software encoding at 1080p would
  burn 4-6x the CPU per peer. 720p is the standard videoconferencing profile
  (PWA setting was "720P @ 30 FPS").
- **Congestion control (REMB):** remote receivers' `ReceiverEstimatedMaximumBitrate`
  is classified per track and drives a frame-skip ratio (send 1 of every N
  frames) plus a recorded encoder bitrate target (openh264 applies it on
  re-initialization). The slowest peer wins; hints expire after 5 s, with a
  200 kbps floor and a 1:5 frame-skip cap.
- **Camera picker:** the top-bar dropdown enumerates webcams (nokhwa `query`),
  persists the choice in `config.json` (`camera_index`), and hot-swaps the
  device mid-call (restarts capture + renegotiates the camera track).
- **Screen-source picker:** the top-bar dropdown lists displays + windows
  (macOS `SCShareableContent`); the choice persists (`screen_source_id`) and
  hot-swaps mid-share. On Linux the XDG portal remains the picker (the entry
  is the system picker itself), and system audio comes from a PipeWire
  default-sink monitor — video-only if the monitor fails.

See [AGENT.md](AGENT.md) for architecture, constraints, and conventions.
