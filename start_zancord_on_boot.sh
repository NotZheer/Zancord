#!/bin/bash
# ZanCord Boot Auto-Start Script for Host Mac (PWA mode)
export PATH=/opt/homebrew/bin:$PATH
cd /Users/zheer/.gemini/antigravity/scratch/Zancord

# Wait up to 15 seconds for Tailscale network interface
for i in {1..15}; do
  if ifconfig | grep -q "100\."; then
    break
  fi
  sleep 1
done

# Kill any stale instances
lsof -i :3000 -i :3443 -i :5173 | grep LISTEN | awk '{print $2}' | xargs kill -9 2>/dev/null || true

# Build the PWA, then serve it (app + signaling) over HTTP/HTTPS
npm run build > build.log 2>&1
npm run server > signaling.log 2>&1 &

# Keep wrapper process alive for macOS launchd
wait
