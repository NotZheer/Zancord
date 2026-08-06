#!/usr/bin/env bash
# Packages zancord-ui into a signed Zancord.app bundle (Phase 6).
#
# macOS screen capture (ScreenCaptureKit) requires a signed binary; without
# this, the app runs but the OS refuses screen-recording permission. Ad-hoc
# signing is enough for local use.
#
# Usage: scripts/package-macos.sh
# Output: zancord-native/target/release/Zancord.app

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_NAME="zancord-ui"
BUNDLE_NAME="Zancord.app"

cd "$WORKSPACE_DIR"
cargo build --release -p zancord-app

APP_DIR="target/release/$BUNDLE_NAME"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

cp "target/release/$BIN_NAME" "$APP_DIR/Contents/MacOS/$BIN_NAME"
cp resources/macos/Info.plist "$APP_DIR/Contents/Info.plist"

# Some macOS installs (e.g. this dev VM) lack the Swift Concurrency runtime in
# the dyld cache, so the binary's @rpath/libswift_Concurrency.dylib fails to
# resolve at launch. Bundle the dylib and point the rpath at it.
SWIFT_RUNTIME=""
for d in \
    "/Library/Developer/CommandLineTools/usr/lib/swift-5.5/macosx" \
    "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"; do
    if [ -f "$d/libswift_Concurrency.dylib" ]; then
        SWIFT_RUNTIME="$d"
        break
    fi
done
if [ -n "$SWIFT_RUNTIME" ]; then
    mkdir -p "$APP_DIR/Contents/Frameworks"
    cp "$SWIFT_RUNTIME/libswift_Concurrency.dylib" "$APP_DIR/Contents/Frameworks/"
    install_name_tool -add_rpath @executable_path/../Frameworks \
        "$APP_DIR/Contents/MacOS/$BIN_NAME" 2>/dev/null || true
fi

# Icons: resources/icons/*.icns (or iconset) if present — none ship yet.
if [ -f resources/icons/icon.icns ]; then
    cp resources/icons/icon.icns "$APP_DIR/Contents/Resources/icon.icns"
fi

# Ad-hoc sign (required for ScreenCaptureKit permission prompts).
codesign --force --sign - "$APP_DIR" >/dev/null
codesign --verify "$APP_DIR" >/dev/null

echo "Built $APP_DIR (ad-hoc signed)"
echo "Run: open $APP_DIR"
echo "Note: the join screen pre-fills ws://127.0.0.1:3000 — pass the"
echo "signaling server's Tailscale IP if it runs on another machine."
