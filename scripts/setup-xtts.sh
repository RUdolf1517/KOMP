#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME_DIR="$ROOT_DIR/vendor/xtts"
VENV_DIR="$RUNTIME_DIR/.venv"
MODEL="${KOMP_XTTS_MODEL:-tts_models/multilingual/multi-dataset/xtts_v2}"

if [[ "${KOMP_XTTS_ACCEPT_CPML:-}" != "1" ]]; then
  echo "XTTS v2 uses the CPML non-commercial license." >&2
  echo "Accept it in KOMP or set KOMP_XTTS_ACCEPT_CPML=1." >&2
  exit 1
fi

PYTHON="${KOMP_XTTS_PYTHON:-}"
if [[ -z "$PYTHON" ]]; then
  for candidate in python3.13 python3.12 python3.11 python3.10 python3; do
    if command -v "$candidate" >/dev/null 2>&1; then PYTHON="$candidate"; break; fi
  done
fi
if [[ -z "$PYTHON" ]]; then
  echo "XTTS v2 requires Python 3.10-3.14." >&2
  exit 1
fi
PYTHON_VERSION="$($PYTHON -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
PYTHON_MINOR="${PYTHON_VERSION#3.}"
if [[ "${PYTHON_VERSION%%.*}" != "3" || "$PYTHON_MINOR" -lt 10 || "$PYTHON_MINOR" -gt 14 ]]; then
  echo "XTTS v2 requires Python 3.10-3.14; found $PYTHON_VERSION at $PYTHON" >&2
  echo "Set KOMP_XTTS_PYTHON to a compatible executable." >&2
  exit 1
fi

mkdir -p "$RUNTIME_DIR"
if [[ ! -x "$VENV_DIR/bin/python" ]]; then "$PYTHON" -m venv "$VENV_DIR"; fi
"$VENV_DIR/bin/python" -m pip install --upgrade "pip<26" "setuptools<81" wheel
"$VENV_DIR/bin/python" -m pip install torch torchaudio
"$VENV_DIR/bin/python" -m pip install coqui-tts "transformers<5.1" fastapi uvicorn numpy soundfile

TTS_HOME="$RUNTIME_DIR/models" COQUI_TOS_AGREED=1 "$VENV_DIR/bin/python" -c "from pathlib import Path; from TTS.api import TTS; TTS(model_name='$MODEL', progress_bar=True); Path(r'$RUNTIME_DIR/model-installed').write_text('$MODEL', encoding='utf-8')"
echo "XTTS v2 installation completed"
