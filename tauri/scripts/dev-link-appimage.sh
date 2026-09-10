#!/usr/bin/env bash
# Points the Decky plugin at the locally-built dev AppImage.
#
# The plugin reads `spool_command` from its settings file on load and uses it
# to launch `spool --headless-server`. This script writes that path and
# restarts the plugin_loader service so the change takes effect immediately.
#
# Run via `bun run dev:link` from tauri/ after `bun run build:appimage`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TAURI_DIR="$(dirname "$SCRIPT_DIR")"
case "$(uname -m)" in
  aarch64 | arm64) BUNDLER_ARCH="aarch64" ;;
  *) BUNDLER_ARCH="amd64" ;;
esac
APPIMAGE="$TAURI_DIR/src-tauri/target/release/bundle/appimage/Spool_${BUNDLER_ARCH}.AppImage"
SETTINGS_DIR="${HOME}/homebrew/settings/spool-backup"
SETTINGS_FILE="$SETTINGS_DIR/settings.json"

if [ ! -f "$APPIMAGE" ]; then
  echo "Error: AppImage not found at $APPIMAGE" >&2
  echo "Run 'bun run build:appimage' first." >&2
  exit 1
fi

mkdir -p "$SETTINGS_DIR"

# Preserve existing settings (e.g. notify), update only spool_command.
if [ -f "$SETTINGS_FILE" ]; then
  jq --arg cmd "$APPIMAGE" '.spool_command = $cmd' "$SETTINGS_FILE" > "$SETTINGS_FILE.tmp" \
    && mv "$SETTINGS_FILE.tmp" "$SETTINGS_FILE"
else
  jq -n --arg cmd "$APPIMAGE" '{"spool_command": $cmd, "notify": true}' > "$SETTINGS_FILE"
fi

echo "spool_command -> $APPIMAGE"

# Restart the plugin loader so the plugin re-reads the settings.
# Try pkexec (GUI auth dialog) first; fall back to a manual hint.
if pkexec systemctl restart plugin_loader 2>/dev/null; then
  echo "plugin_loader restarted."
else
  echo ""
  echo "Restart plugin_loader to pick up the change:"
  echo "  sudo systemctl restart plugin_loader"
fi
