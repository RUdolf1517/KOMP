#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR_DIR="$ROOT_DIR/vendor/vosk"
MODELS_DIR="$VENDOR_DIR/models"
CONFIG_PATH="$ROOT_DIR/komp.prototype.toml"

RU_MODEL_VERSION="${EREZ_RU_MODEL_VERSION:-0.22}"
EN_MODEL_VERSION="${EREZ_EN_MODEL_VERSION:-0.15}"
RU_MODEL_URL="https://alphacephei.com/vosk/models/vosk-model-small-ru-${RU_MODEL_VERSION}.zip"
EN_MODEL_URL="https://alphacephei.com/vosk/models/vosk-model-small-en-us-${EN_MODEL_VERSION}.zip"

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Required command not found: $1" >&2
    exit 1
  fi
}

download_model() {
  local lang="$1"
  local url="$2"
  local dest="$MODELS_DIR/$lang"
  local tmp="$ROOT_DIR/target/vosk-model-$lang"
  local archive="$tmp/model.zip"

  if [[ -f "$dest/am/final.mdl" || -f "$dest/conf/model.conf" ]]; then
    echo "Model $lang already installed at $dest"
    return
  fi

  echo "Downloading $lang model from $url"
  mkdir -p "$tmp" "$MODELS_DIR"
  curl -L "$url" -o "$archive"
  rm -rf "$tmp/extract" "$dest"
  mkdir -p "$tmp/extract"
  unzip -q "$archive" -d "$tmp/extract"
  local extracted
  extracted="$(find "$tmp/extract" -maxdepth 1 -mindepth 1 -type d | head -n 1)"
  if [[ -z "$extracted" ]]; then
    echo "Model archive did not contain a model directory" >&2
    exit 1
  fi
  mv "$extracted" "$dest"
  echo "Installed $lang model into $dest"
}

need_cmd cargo
need_cmd curl
need_cmd unzip
need_cmd git

"$ROOT_DIR/scripts/auto-update-git.sh"

if [[ ! -f "$VENDOR_DIR/lib/libvosk.dylib" ]]; then
  "$ROOT_DIR/scripts/setup-vosk-macos.sh"
else
  echo "Native Vosk library already installed at $VENDOR_DIR/lib/libvosk.dylib"
fi

download_model "ru" "$RU_MODEL_URL"
download_model "en" "$EN_MODEL_URL"

if [[ ! -f "$CONFIG_PATH" ]]; then
cat > "$CONFIG_PATH" <<EOF
wake_phrase = "комп"
wake_phrases = ["комп", "компьютер"]
wake_grammar = ["комп", "компьютер"]
primary_language = "ru"
english_fallback = true
plugin_dirs = ["plugins.example"]

[models]
ru_vosk_path = "$MODELS_DIR/ru"
en_vosk_path = "$MODELS_DIR/en"

[lmstudio]
enabled = false
base_url = "http://localhost:1234/v1"
model = "local-model"
timeout_ms = 2500
min_confidence = 0.55

[whisper]
enabled = false
# cli_path = "vendor/whisper.cpp/build/bin/whisper-cli"
# model_path = "vendor/whisper.cpp/models/ggml-base.bin"
language = "ru"
timeout_ms = 8000
extra_args = ["-nt"]

[tts]
enabled = false
provider = "cosyvoice"
base_url = "http://127.0.0.1:50000"
model_path = "vendor/cosyvoice/models/Fun-CosyVoice3-0.5B"
voice_id = "komp"
autostart = true
preload = true
timeout_ms = 180000
cache_enabled = true
device = "auto"
playback_mode = "buffered"

[audio]
sample_rate_hz = 16000
command_timeout_ms = 10000
end_silence_ms = 1200
command_preroll_ms = 300

[sounds]
startup = "sounds/system/startup.mp3"
shutdown = "sounds/system/shutdown.mp3"
wake = "sounds/system/listening.mp3"
listening = "sounds/system/listening.mp3"
EOF
else
  echo "Using existing config at $CONFIG_PATH"
fi

export DYLD_LIBRARY_PATH="$VENDOR_DIR/lib:${DYLD_LIBRARY_PATH:-}"
export KOMP_CONFIG="$CONFIG_PATH"
export EREZ_CONFIG="$CONFIG_PATH"
export KOMP_AUTOSTART=1
export EREZ_AUTOSTART=1
export RUST_LOG="${KOMP_RUST_LOG:-${EREZ_RUST_LOG:-komp_daemon=info,erez_daemon=info,erez_core=info}}"

echo
echo "Starting KOMP prototype."
echo "Say: комп ... then speak a command. Logs will appear below."
echo "Config: $CONFIG_PATH"
echo

exec cargo run -p erez-daemon --features live-vosk
