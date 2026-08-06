#!/usr/bin/env bash
# Installs zancord-ui for the current user on Linux (Phase 6): binary into
# ~/.local/bin, launcher into ~/.local/share/applications.
#
# Usage: scripts/package-linux.sh [--system]
#   --system  install into /usr/local/bin + /usr/share/applications (sudo)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_NAME="zancord-ui"

USE_SYSTEM=false
[ "${1:-}" = "--system" ] && USE_SYSTEM=true

cd "$WORKSPACE_DIR"
cargo build --release -p zancord-app

if $USE_SYSTEM; then
    BIN_DIR="/usr/local/bin"
    APPS_DIR="/usr/share/applications"
else
    BIN_DIR="$HOME/.local/bin"
    APPS_DIR="$HOME/.local/share/applications"
fi
mkdir -p "$BIN_DIR" "$APPS_DIR"

install -m 0755 "target/release/$BIN_NAME" "$BIN_DIR/$BIN_NAME"

# Icon (optional — ships none yet; drop the line once one exists).
ICON_LINE=""
if [ -f resources/icons/icon.png ]; then
    ICON_DIR="$APPS_DIR/../icons/hicolor/256x256/apps"
    mkdir -p "$ICON_DIR"
    install -m 0644 resources/icons/icon.png "$ICON_DIR/zancord.png"
    ICON_LINE="Icon=zancord"
fi

DESKTOP_FILE="$APPS_DIR/zancord.desktop"
cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=Zancord
Comment=Tailscale-native P2P voice, video & screen share
Exec=$BIN_DIR/$BIN_NAME
Terminal=false
Categories=Network;AudioVideo;Chat;
$ICON_LINE
EOF

echo "Installed zancord-ui"
echo "  Binary:  $BIN_DIR/$BIN_NAME"
echo "  Launcher: $DESKTOP_FILE"
