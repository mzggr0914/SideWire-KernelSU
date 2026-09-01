#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SKIP_BUILD=0
if [ "${1:-}" = "--skip-build" ]; then
  SKIP_BUILD=1
fi

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target-linux}"

if [ "$SKIP_BUILD" -eq 0 ]; then
  bash "$ROOT/scripts/build-linux.sh"
fi

VERSION="$(awk -F'"' '/^version = "/ { print $2; exit }' Cargo.toml)"
if [ -z "$VERSION" ]; then
  echo "Could not read workspace version from Cargo.toml" >&2
  exit 1
fi
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
esac

SOURCE="$CARGO_TARGET_DIR/release/sidewire"
if [ ! -x "$SOURCE" ]; then
  echo "Linux binary not found: $SOURCE" >&2
  exit 1
fi

mkdir -p "$ROOT/dist"
DEST="$ROOT/dist/sidewire-v${VERSION}-linux-${ARCH}"
cp "$SOURCE" "$DEST"
chmod +x "$DEST"

echo "==> Linux release artifact"
ls -lh "$DEST"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$DEST"
fi
