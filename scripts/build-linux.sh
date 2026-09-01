#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust is not installed. Run ./scripts/setup-linux.sh first." >&2
  exit 1
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target-linux}"

echo "==> Building Linux CLI ($(uname -m))"
cargo build --release -p sidewire

echo "==> Linux build complete"
echo "$CARGO_TARGET_DIR/release/sidewire"
