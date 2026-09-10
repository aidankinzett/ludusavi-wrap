#!/usr/bin/env bash
# Builds the ludusavi sidecar from source.
#
# ludusavi publishes prebuilt binaries for x86_64 Linux, Windows and macOS, but
# not for aarch64 Linux — its release matrix has a single
# `x86_64-unknown-linux-gnu` Linux entry. `download-sidecars.js` fetches the
# prebuilt binary where one exists; this script covers the targets where none
# does, by compiling ludusavi (pure Rust, MIT) at the same pinned version.
#
# The version is read from `download-sidecars.js` so the pin lives in exactly
# one place — bumping LUDUSAVI_VER there is enough for both paths.
#
# Usage:
#   ./build-ludusavi.sh                       # build for the host triple
#   ./build-ludusavi.sh aarch64-unknown-linux-gnu
#
# Build dependencies (matching ludusavi's own Linux CI step):
#   gcc libxcb-composite0-dev libgtk-3-dev
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINARIES_DIR="$SCRIPT_DIR/../src-tauri/binaries"

# Single source of truth for the pin: the `const LUDUSAVI_VER = '...'` line.
VERSION="$(sed -n "s/^const LUDUSAVI_VER *= *'\([^']*\)'.*/\1/p" \
  "$SCRIPT_DIR/download-sidecars.js")"
if [ -z "$VERSION" ]; then
  echo "error: could not read LUDUSAVI_VER from download-sidecars.js" >&2
  exit 1
fi

HOST="$(rustc -vV | sed -n 's/^host: //p')"
TARGET="${1:-$HOST}"
if [ -z "$TARGET" ]; then
  echo "error: could not determine the Rust target triple (is rustc on PATH?)" >&2
  exit 1
fi
# The argument names an output file, so reject anything that isn't a triple
# rather than writing out `ludusavi---help`. `case` takes the first matching
# branch, so the leading-dash check has to come before the triple shape —
# `--help` satisfies `*-*-*` on its own.
target_ok=0
case "$TARGET" in
  -* | *' '*) ;;
  *-*-*) target_ok=1 ;;
esac
if [ "$target_ok" -ne 1 ]; then
  echo "error: '$TARGET' is not a Rust target triple" >&2
  echo "usage: build-ludusavi.sh [target-triple]" >&2
  exit 1
fi

DEST="$BINARIES_DIR/ludusavi-$TARGET"

echo "==> Building ludusavi v$VERSION for $TARGET"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

git clone --depth 1 --branch "v$VERSION" \
  https://github.com/mtkennerly/ludusavi.git "$WORK/ludusavi"

# Build for the host triple by default; pass --target only when
# cross-compiling, so a plain host build doesn't need the target installed.
if [ "$TARGET" = "$HOST" ]; then
  cargo build --release --manifest-path "$WORK/ludusavi/Cargo.toml"
  BUILT="$WORK/ludusavi/target/release/ludusavi"
else
  cargo build --release --target "$TARGET" \
    --manifest-path "$WORK/ludusavi/Cargo.toml"
  BUILT="$WORK/ludusavi/target/$TARGET/release/ludusavi"
fi

mkdir -p "$BINARIES_DIR"
install -m 755 "$BUILT" "$DEST"

echo "==> Installed $DEST"

# Smoke-test only a native build — a cross-compiled binary can't run here.
if [ "$TARGET" = "$HOST" ]; then
  "$DEST" --version
fi
