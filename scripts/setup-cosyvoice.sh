#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME_DIR="$ROOT_DIR/vendor/cosyvoice"
SOURCE_DIR="$RUNTIME_DIR/source"
VENV_DIR="$RUNTIME_DIR/.venv"
MODEL_DIR="$RUNTIME_DIR/models/Fun-CosyVoice3-0.5B"
REPOSITORY="https://github.com/FunAudioLLM/CosyVoice.git"
MODEL="FunAudioLLM/Fun-CosyVoice3-0.5B-2512"

PYTHON="${KOMP_COSYVOICE_PYTHON:-}"
if [[ -z "$PYTHON" ]]; then
  for candidate in python3.10 python3; do
    if command -v "$candidate" >/dev/null 2>&1; then PYTHON="$candidate"; break; fi
  done
fi
PYTHON_VERSION="$($PYTHON -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
if [[ "$PYTHON_VERSION" != "3.10" ]]; then
  echo "CosyVoice requires Python 3.10; found $PYTHON_VERSION at $PYTHON" >&2
  echo "Set KOMP_COSYVOICE_PYTHON to a Python 3.10 executable." >&2
  exit 1
fi
if [[ -z "$PYTHON" ]]; then
  echo "Python 3.10 is required for CosyVoice" >&2
  exit 1
fi

mkdir -p "$RUNTIME_DIR/models"
if [[ ! -d "$SOURCE_DIR/.git" ]]; then
  echo "Cloning CosyVoice..."
  git clone --recursive "$REPOSITORY" "$SOURCE_DIR"
else
  echo "CosyVoice source already installed"
  git -C "$SOURCE_DIR" submodule update --init --recursive
fi

if [[ ! -x "$VENV_DIR/bin/python" ]]; then
  "$PYTHON" -m venv "$VENV_DIR"
fi
"$VENV_DIR/bin/python" -m pip install --upgrade "pip<25" "setuptools<81" wheel packaging
"$VENV_DIR/bin/python" -m pip install --no-build-isolation openai-whisper==20231117
"$VENV_DIR/bin/python" -m pip install -r "$SOURCE_DIR/requirements.txt"
"$VENV_DIR/bin/python" -m pip install huggingface_hub

if [[ ! -d "$MODEL_DIR" ]]; then
  echo "Downloading $MODEL..."
  "$VENV_DIR/bin/python" -c "from huggingface_hub import snapshot_download; snapshot_download('$MODEL', local_dir=r'$MODEL_DIR')"
else
  echo "CosyVoice model already installed"
fi

echo "CosyVoice installation completed"
