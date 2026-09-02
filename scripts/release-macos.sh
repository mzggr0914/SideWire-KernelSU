#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SKIP_BUILD=0
if [ "${1:-}" = "--skip-build" ]; then
  SKIP_BUILD=1
fi

if [ "$(uname -s)" != "Darwin" ]; then
  echo "release-macos.sh must run on macOS" >&2
  exit 1
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target-macos}"
if [ "$SKIP_BUILD" -eq 0 ]; then
  bash "$ROOT/scripts/build-macos.sh"
fi

VERSION="$(awk -F'"' '/^version = "/ { print $2; exit }' Cargo.toml)"
if [ -z "$VERSION" ]; then
  echo "Could not read workspace version from Cargo.toml" >&2
  exit 1
fi

ARCH="$(uname -m)"
case "$ARCH" in
  arm64|aarch64) ARCH=arm64 ;;
  x86_64|amd64) ARCH=x86_64 ;;
  *) echo "Unsupported macOS architecture: $ARCH" >&2; exit 1 ;;
esac

SOURCE="$CARGO_TARGET_DIR/release/sidewire"
if [ ! -x "$SOURCE" ]; then
  echo "macOS binary not found: $SOURCE" >&2
  exit 1
fi
mkdir -p "$ROOT/dist"
DEST="$ROOT/dist/sidewire-v${VERSION}-macos-${ARCH}"
cp "$SOURCE" "$DEST"
chmod +x "$DEST"

echo "==> macOS release artifact"
ls -lh "$DEST"
if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 "$DEST"
fi
