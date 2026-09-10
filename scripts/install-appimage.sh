#!/usr/bin/env bash
# Installs Spool on Linux from the latest GitHub release AppImage.
#
# Downloads the AppImage into ~/Applications, then registers it with the
# desktop so it shows up in the application launcher (KDE Plasma, GNOME, etc.)
# with its icon. The AppImage updates itself in place, so this only needs to
# run once; re-running it pulls the current release.
#
# Usage:
#   ./install-appimage.sh              # install / update
#   ./install-appimage.sh --uninstall  # remove the AppImage + launcher entry
#
# One-liner (no clone needed):
#   curl -fsSL https://raw.githubusercontent.com/aidankinzett/Spool/master/scripts/install-appimage.sh | bash
#
# Env overrides:
#   INSTALL_DIR   where the AppImage lands (default: ~/Applications)
set -euo pipefail

REPO="aidankinzett/Spool"

# Spool publishes one AppImage per architecture. Pick the one matching this
# machine so an ARM handheld doesn't silently install the x86_64 build.
case "$(uname -m)" in
  x86_64 | amd64) ASSET="Spool_amd64.AppImage" ;;
  aarch64 | arm64) ASSET="Spool_aarch64.AppImage" ;;
  *)
    echo "Unsupported architecture: $(uname -m)." >&2
    echo "Spool ships AppImages for x86_64 and aarch64 only." >&2
    exit 1
    ;;
esac

DOWNLOAD_URL="https://github.com/$REPO/releases/latest/download/$ASSET"

INSTALL_DIR="${INSTALL_DIR:-$HOME/Applications}"
APPIMAGE_PATH="$INSTALL_DIR/$ASSET"
DESKTOP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
DESKTOP_PATH="$DESKTOP_DIR/spool.desktop"
ICON_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"

# ── Refresh the desktop/icon caches so the launcher notices changes ───────────
refresh_caches() {
  if command -v update-desktop-database &>/dev/null; then
    update-desktop-database "$DESKTOP_DIR" &>/dev/null || true
  fi
  if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -f -t "$ICON_ROOT" &>/dev/null || true
  fi
  # KDE Plasma maintains its own service/icon cache separate from GTK's.
  # Try versioned binaries first, then the unversioned fallback.
  local kbuildsycoca
  kbuildsycoca="$(command -v kbuildsycoca6 || command -v kbuildsycoca5 || command -v kbuildsycoca || true)"
  if [ -n "$kbuildsycoca" ]; then
    "$kbuildsycoca" --noincremental &>/dev/null || true
  fi
}

# ── Uninstall ─────────────────────────────────────────────────────────────────
if [ "${1:-}" = "--uninstall" ]; then
  echo "==> Removing Spool..."
  rm -f "$APPIMAGE_PATH" "$DESKTOP_PATH"
  find "$ICON_ROOT" -name 'spool.png' -delete 2>/dev/null || true
  refresh_caches
  echo "Done. Spool has been removed."
  exit 0
fi

# ── Download the AppImage ─────────────────────────────────────────────────────
mkdir -p "$INSTALL_DIR"
echo "==> Downloading the latest Spool AppImage..."
echo "    $DOWNLOAD_URL"
TMP_DL="$(mktemp "$INSTALL_DIR/.Spool.XXXXXX.AppImage")"
trap 'rm -f "$TMP_DL"' EXIT
if command -v curl &>/dev/null; then
  curl -fL --progress-bar -o "$TMP_DL" "$DOWNLOAD_URL"
elif command -v wget &>/dev/null; then
  wget -q --show-progress -O "$TMP_DL" "$DOWNLOAD_URL"
else
  echo "Error: need curl or wget to download." >&2
  exit 1
fi
chmod +x "$TMP_DL"
mv -f "$TMP_DL" "$APPIMAGE_PATH"
trap - EXIT
echo "    Installed to $APPIMAGE_PATH"

# ── Extract the bundled .desktop entry and icon ───────────────────────────────
# AppImages carry their own desktop entry + icons. We reuse them (so things like
# StartupWMClass stay correct) but rewrite Exec/Icon to point at the installed
# copy rather than the transient mount path.
echo "==> Registering with the application launcher..."
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
(
  cd "$WORK"
  # --appimage-extract unpacks without needing FUSE.
  "$APPIMAGE_PATH" --appimage-extract >/dev/null 2>&1
)
SQUASH="$WORK/squashfs-root"

SRC_DESKTOP="$(find "$SQUASH" -maxdepth 1 -name '*.desktop' | head -1 || true)"
ICON_NAME="spool"
if [ -n "$SRC_DESKTOP" ]; then
  EMBEDDED_ICON="$(grep -m1 '^Icon=' "$SRC_DESKTOP" | cut -d= -f2- || true)"
fi
EMBEDDED_ICON="${EMBEDDED_ICON:-spool}"

# ── Install icons at every size the AppImage provides ─────────────────────────
# Icons must be installed before the .desktop entry is written so we can use
# an absolute path for Icon=. KDE Plasma's taskbar on Wayland doesn't resolve
# icon theme names the same way the app menu does; an absolute path works
# reliably on both.
INSTALLED_ICON=0
BEST_ICON_PATH=""
BEST_ICON_SIZE=0
while IFS= read -r png; do
  size_dir="$(basename "$(dirname "$(dirname "$png")")")"  # e.g. 128x128
  dest="$ICON_ROOT/$size_dir/apps"
  mkdir -p "$dest"
  cp -f "$png" "$dest/$ICON_NAME.png"
  INSTALLED_ICON=1
  # Track the largest size for use as the absolute Icon= path.
  size_px="${size_dir%%x*}"
  size_px="${size_px%%@*}"
  if [[ "$size_px" =~ ^[0-9]+$ ]] && [ "$size_px" -gt "$BEST_ICON_SIZE" ]; then
    BEST_ICON_SIZE="$size_px"
    BEST_ICON_PATH="$dest/$ICON_NAME.png"
  fi
done < <(find "$SQUASH/usr/share/icons/hicolor" -type f -name "${EMBEDDED_ICON}.png" 2>/dev/null)

# Fall back to the top-level .DirIcon if no themed icons were found.
if [ "$INSTALLED_ICON" -eq 0 ]; then
  DIRICON="$(find "$SQUASH" -maxdepth 1 -name '.DirIcon' | head -1 || true)"
  if [ -n "$DIRICON" ]; then
    dest="$ICON_ROOT/256x256/apps"
    mkdir -p "$dest"
    cp -fL "$DIRICON" "$dest/$ICON_NAME.png"
    BEST_ICON_PATH="$dest/$ICON_NAME.png"
  fi
fi

# Use absolute icon path if we installed one, otherwise fall back to theme name.
ICON_VALUE="${BEST_ICON_PATH:-$ICON_NAME}"

mkdir -p "$DESKTOP_DIR"
if [ -n "$SRC_DESKTOP" ]; then
  # Reuse the embedded entry, rewriting Exec/Icon and the path-based TryExec.
  WM_CLASS_NAME="$(grep -m1 '^Name=' "$SRC_DESKTOP" | cut -d= -f2-)"
  WM_CLASS_NAME="${WM_CLASS_NAME:-Spool}"
  sed -E \
    -e "s#^Exec=.*#Exec=\"$APPIMAGE_PATH\" %U#" \
    -e "s#^Icon=.*#Icon=$ICON_VALUE#" \
    -e "s#^StartupWMClass=.*#StartupWMClass=$WM_CLASS_NAME#" \
    -e "/^TryExec=/d" \
    "$SRC_DESKTOP" > "$DESKTOP_PATH"
else
  # Fallback entry if the AppImage shipped without one.
  cat > "$DESKTOP_PATH" <<EOF
[Desktop Entry]
Type=Application
Name=Spool
Comment=Game library + save-management wrapper
Exec="$APPIMAGE_PATH" %U
Icon=$ICON_VALUE
Terminal=false
Categories=Game;
StartupWMClass=Spool
EOF
fi
chmod +x "$DESKTOP_PATH"

refresh_caches

echo ""
echo "Done. Spool is in your application launcher."
echo "  AppImage: $APPIMAGE_PATH"
echo "  Launcher: $DESKTOP_PATH"
echo ""
echo "It auto-updates in place; re-run this script anytime to reinstall."
