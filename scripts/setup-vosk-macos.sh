#!/usr/bin/env bash
set -euo pipefail

VERSION="${VOSK_VERSION:-0.3.44}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR_DIR="$ROOT_DIR/vendor/vosk"
TMP_DIR="$ROOT_DIR/target/vosk-setup"
ARCHIVE="$TMP_DIR/vosk-macos-${VERSION}.whl"

case "$VERSION" in
  0.3.44)
    URL="https://files.pythonhosted.org/packages/44/19/5e8299237bc2005c3d155d3b48adba6fd6484465ad5c970302fc1d37947d/vosk-0.3.44-py3-none-macosx_10_6_universal2.whl"
    ;;
  *)
    URL="https://pypi.org/packages/source/v/vosk/vosk-${VERSION}.tar.gz"
    echo "Unsupported automatic macOS native Vosk version: $VERSION" >&2
    echo "Set VOSK_VERSION=0.3.44 or install libvosk.dylib manually into $VENDOR_DIR/lib" >&2
    exit 1
    ;;
esac

mkdir -p "$VENDOR_DIR/lib" "$VENDOR_DIR/include" "$TMP_DIR"

echo "Downloading $URL"
curl -L "$URL" -o "$ARCHIVE"
rm -rf "$TMP_DIR/extract"
mkdir -p "$TMP_DIR/extract"
unzip -q "$ARCHIVE" -d "$TMP_DIR/extract"

LIB="$(find "$TMP_DIR/extract" \( -name 'libvosk.dylib' -o -name 'libvosk.dyld' \) -type f | head -n 1)"
if [[ -z "$LIB" ]]; then
  echo "libvosk.dylib/libvosk.dyld was not found in archive" >&2
  exit 1
fi

cp "$LIB" "$VENDOR_DIR/lib/libvosk.dylib"
find "$TMP_DIR/extract" -name 'vosk_api.h' -type f -exec cp {} "$VENDOR_DIR/include/" \; -quit

echo "Installed native Vosk library into $VENDOR_DIR/lib"
echo "For runtime, use:"
echo "  export DYLD_LIBRARY_PATH=\"$VENDOR_DIR/lib:\${DYLD_LIBRARY_PATH:-}\""
