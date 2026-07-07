#!/usr/bin/env bash
set -euo pipefail

MIN_RUST_MAJOR=1
MIN_RUST_MINOR=85

fail() {
  echo "$1" >&2
  return 1 2>/dev/null || exit 1
}

cargo_version() {
  local line version
  line="$(cargo --version)"
  version="${line#cargo }"
  version="${version%% *}"
  echo "$version"
}

version_is_supported() {
  local version="$1"
  local major minor rest
  major="${version%%.*}"
  rest="${version#*.}"
  minor="${rest%%.*}"

  if [[ "$major" -gt "$MIN_RUST_MAJOR" ]]; then
    return 0
  fi
  if [[ "$major" -eq "$MIN_RUST_MAJOR" && "$minor" -ge "$MIN_RUST_MINOR" ]]; then
    return 0
  fi
  return 1
}

if ! command -v cargo >/dev/null 2>&1; then
  fail "Required command not found: cargo. Install Rust from https://rustup.rs/"
fi

if [[ -x "$HOME/.cargo/bin/cargo" ]]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi

CURRENT_CARGO_VERSION="$(cargo_version)"
if version_is_supported "$CURRENT_CARGO_VERSION"; then
  return 0 2>/dev/null || exit 0
fi

echo "Cargo $CURRENT_CARGO_VERSION is too old for KOMP dependencies." >&2
echo "KOMP requires Rust/Cargo ${MIN_RUST_MAJOR}.${MIN_RUST_MINOR}+ because some crates use Rust 2024." >&2

if command -v rustup >/dev/null 2>&1; then
  echo "Updating Rust stable toolchain with rustup..." >&2
  rustup toolchain install stable
  rustup default stable
  export PATH="$HOME/.cargo/bin:$PATH"
  hash -r 2>/dev/null || true

  CURRENT_CARGO_VERSION="$(cargo_version)"
  if version_is_supported "$CURRENT_CARGO_VERSION"; then
    echo "Rust toolchain is ready: cargo $CURRENT_CARGO_VERSION" >&2
    return 0 2>/dev/null || exit 0
  fi
fi

fail "Please install a current Rust toolchain: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
