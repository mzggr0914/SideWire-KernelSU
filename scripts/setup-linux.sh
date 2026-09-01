#!/usr/bin/env bash
set -euo pipefail

if ! command -v cc >/dev/null 2>&1; then
  echo "A C compiler is required. On Ubuntu: sudo apt install build-essential" >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  if ! command -v curl >/dev/null 2>&1; then
    echo "curl is required to install rustup." >&2
    exit 1
  fi
  echo "==> Installing Rust stable with rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
fi

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi

rustup component add clippy rustfmt
rustc --version
cargo --version
