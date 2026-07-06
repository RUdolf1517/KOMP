#!/usr/bin/env bash
set -euo pipefail

PACKAGES=(
  curl
  unzip
  xdg-utils
  libasound2-dev
  pkg-config
)

if ! command -v apt-get >/dev/null 2>&1; then
  echo "This helper targets Ubuntu/Debian systems with apt-get." >&2
  echo "Install these packages manually: ${PACKAGES[*]}" >&2
  exit 1
fi

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  apt-get update
  apt-get install -y "${PACKAGES[@]}"
else
  sudo apt-get update
  sudo apt-get install -y "${PACKAGES[@]}"
fi
