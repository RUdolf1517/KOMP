#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export LD_LIBRARY_PATH="$ROOT_DIR/vendor/vosk/lib:${LD_LIBRARY_PATH:-}"
exec cargo run -p erez-daemon --features live-vosk
