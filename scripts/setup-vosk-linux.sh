#!/usr/bin/env bash
set -euo pipefail

VERSION="${VOSK_VERSION:-0.3.45}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR_DIR="$ROOT_DIR/vendor/vosk"
TMP_DIR="$ROOT_DIR/target/vosk-setup"

case "$(uname -m)" in
  x86_64 | amd64)
    PLATFORM="linux-x86_64"
    ;;
  aarch64 | arm64)
    PLATFORM="linux-aarch64"
    ;;
  *)
    echo "Unsupported Linux architecture: $(uname -m)" >&2
    echo "Install libvosk.so manually into $VENDOR_DIR/lib" >&2
    exit 1
    ;;
esac

ARCHIVE="$TMP_DIR/vosk-$PLATFORM-$VERSION.zip"
URL="https://github.com/alphacep/vosk-api/releases/download/v$VERSION/vosk-$PLATFORM-$VERSION.zip"

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Required command not found: $1" >&2
    case "$1" in
      curl | unzip)
        echo "On Ubuntu/Debian: sudo apt install curl unzip" >&2
        ;;
    esac
    exit 1
  fi
}

need_cmd curl
need_cmd unzip

mkdir -p "$VENDOR_DIR/lib" "$VENDOR_DIR/include" "$TMP_DIR"

echo "Downloading $URL"
curl -L "$URL" -o "$ARCHIVE"
rm -rf "$TMP_DIR/extract"
mkdir -p "$TMP_DIR/extract"
unzip -q "$ARCHIVE" -d "$TMP_DIR/extract"

LIB="$(find "$TMP_DIR/extract" -name 'libvosk.so' -type f | head -n 1)"
HEADER="$(find "$TMP_DIR/extract" -name 'vosk_api.h' -type f | head -n 1)"
if [[ -z "$LIB" ]]; then
  echo "libvosk.so was not found in archive" >&2
  exit 1
fi

cp "$LIB" "$VENDOR_DIR/lib/libvosk.so"
if [[ -n "$HEADER" ]]; then
  cp "$HEADER" "$VENDOR_DIR/include/vosk_api.h"
fi

echo "Installed native Vosk library into $VENDOR_DIR/lib"
echo "For runtime, use:"
echo "  export LD_LIBRARY_PATH=\"$VENDOR_DIR/lib:\${LD_LIBRARY_PATH:-}\""
