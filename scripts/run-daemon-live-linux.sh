#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export LD_LIBRARY_PATH="$ROOT_DIR/vendor/vosk/lib:${LD_LIBRARY_PATH:-}"
export LIBRARY_PATH="$ROOT_DIR/vendor/vosk/lib:${LIBRARY_PATH:-}"
export RUSTFLAGS="-L native=$ROOT_DIR/vendor/vosk/lib ${RUSTFLAGS:-}"
exec cargo run -p erez-daemon --features live-vosk
