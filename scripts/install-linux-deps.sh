#!/usr/bin/env bash
set -euo pipefail

PACKAGES=(
  curl
  unzip
  xdg-utils
  build-essential
  libasound2-dev
  libayatana-appindicator3-dev
  libgtk-3-dev
  librsvg2-dev
  pkg-config
)

WEBKIT_PACKAGE="libwebkit2gtk-4.1-dev"

if ! command -v apt-get >/dev/null 2>&1; then
  echo "This helper targets Ubuntu/Debian systems with apt-get." >&2
  echo "Install these packages manually: ${PACKAGES[*]} $WEBKIT_PACKAGE" >&2
  exit 1
fi

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  apt-get update
  if ! apt-cache show "$WEBKIT_PACKAGE" >/dev/null 2>&1; then
    WEBKIT_PACKAGE="libwebkit2gtk-4.0-dev"
  fi
  apt-get install -y "${PACKAGES[@]}" "$WEBKIT_PACKAGE"
else
  sudo apt-get update
  if ! apt-cache show "$WEBKIT_PACKAGE" >/dev/null 2>&1; then
    WEBKIT_PACKAGE="libwebkit2gtk-4.0-dev"
  fi
  sudo apt-get install -y "${PACKAGES[@]}" "$WEBKIT_PACKAGE"
fi
