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

See [AGENT.md](AGENT.md) for architecture, constraints, and conventions.
