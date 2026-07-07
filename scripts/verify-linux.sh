#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Required command not found: $1" >&2
    case "$1" in
      cargo)
        echo "Install Rust from https://rustup.rs/" >&2
        ;;
      curl | unzip | xdg-open)
        echo "On Ubuntu/Debian: sudo apt install curl unzip xdg-utils" >&2
        ;;
      pkg-config)
        echo "On Ubuntu/Debian: run ./scripts/install-linux-deps.sh" >&2
        ;;
    esac
    exit 1
  fi
}

need_cmd cargo
need_cmd curl
need_cmd unzip
need_cmd xdg-open
need_cmd pkg-config

cd "$ROOT_DIR"

bash -n scripts/install-linux-deps.sh scripts/setup-vosk-linux.sh scripts/run-prototype-linux.sh scripts/run-daemon-live-linux.sh scripts/verify-linux.sh
cargo fmt --check
cargo test

if [[ ! -f "$ROOT_DIR/vendor/vosk/lib/libvosk.so" ]]; then
  "$ROOT_DIR/scripts/setup-vosk-linux.sh"
fi

export LD_LIBRARY_PATH="$ROOT_DIR/vendor/vosk/lib:${LD_LIBRARY_PATH:-}"
export LIBRARY_PATH="$ROOT_DIR/vendor/vosk/lib:${LIBRARY_PATH:-}"
export RUSTFLAGS="-L native=$ROOT_DIR/vendor/vosk/lib ${RUSTFLAGS:-}"
cargo build -p erez-daemon --features live-vosk

echo "Ubuntu/Debian verification passed."
