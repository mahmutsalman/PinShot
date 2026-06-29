#!/usr/bin/env bash
# Build PinShot for production and install it to /Applications so the Dock icon
# launches the new version. One command: build → quit → copy → RE-SIGN → launch.
#
# THE GOTCHA this script exists for: copying a Tauri .app into /Applications with
# `ditto`/`cp` BREAKS its ad-hoc code signature ("code has no resources but
# signature indicates they must be present"), and the app then silently refuses
# to launch. We must `codesign --force --deep --sign -` the copy IN PLACE.
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME="PinShot.app"
BUILT="src-tauri/target/release/bundle/macos/$APP_NAME"
DEST="/Applications/$APP_NAME"

echo "▶ Building (npm run tauri build) — Rust release compile…"
npm run tauri build

echo "▶ Quitting any running PinShot…"
osascript -e 'quit app "PinShot"' 2>/dev/null || true
pkill -x pinshot 2>/dev/null || true   # the binary is lowercase 'pinshot'
sleep 1

echo "▶ Installing to ${DEST} ..."
rm -rf "$DEST"
ditto "$BUILT" "$DEST"

echo "▶ Re-signing ad-hoc (required — ditto/cp break the bundle signature)…"
codesign --force --deep --sign - "$DEST"
codesign --verify --deep --strict "$DEST" && echo "  signature OK"

echo "▶ Launching…"
open -a PinShot

echo "✅ Installed + launched: $DEST"
