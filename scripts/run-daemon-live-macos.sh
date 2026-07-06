#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export DYLD_LIBRARY_PATH="$ROOT_DIR/vendor/vosk/lib:${DYLD_LIBRARY_PATH:-}"
exec cargo run -p erez-daemon --features live-vosk
