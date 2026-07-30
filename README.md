# ZanCord 🚀

**ZanCord** is a zero-telemetry, ultra-low-latency 1080p 60FPS P2P video call, screen share, and encrypted voice chat application built with **Tauri v2 (Rust)**, **WebRTC**, and **Node.js**.

---

## ✨ Features
- **Zero Telemetry & 100% P2P**: Direct Peer-to-Peer encrypted audio, video, and screen sharing.
- **Ultra-Sleek Dark Glassmorphism UI**: Custom Discord-inspired 16:9 call grid with custom matte dark styling.
- **Tailscale Mesh Integration**: Auto-detects Tailscale P2P IP for zero-config direct connections across networks.
- **Linux WebKit Auto-Permissions**: Native Linux GTK permission handler auto-grants camera & mic access.
- **Noise Gate & Low-Cut Audio Filter**: Built-in 80Hz high-pass filter & client-side noise suppression.

---

## 🚀 How to Run Locally

### 1️⃣ Install Dependencies
```bash
npm install
```

### 2️⃣ Run Signaling Server
```bash
npm start
```
The server will start on:
- HTTP: `http://localhost:3000`
- HTTPS: `https://localhost:3443`

### 3️⃣ Build Linux Desktop Package (`.deb`)
```bash
npm run tauri build -- --bundles deb
```

---

## 👥 How a Friend Joins from Web
1. Open the app and click **`Copy Link`** at the top right.
2. Send the link to your friend (e.g., `https://<YOUR_TAILSCALE_IP>:3443/#room=duo-cinema-room`).
3. Your friend opens the link in **Chrome, Safari, or Edge** on Mac/Windows/Linux/Mobile and clicks **Allow** on Camera & Mic!
