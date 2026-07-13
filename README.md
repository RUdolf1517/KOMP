# KOMP Backend

Rust backend skeleton for an offline RU/EN voice assistant with wake phrase `комп`, local plugin commands, scenarios, MP3 responses, and optional LM Studio fallback.

## What Works In This V1

- Rust workspace with `erez-core`, `erez-daemon`, `erez-cli`, and the Tauri `komp-desktop` app.
- Config defaults for wake phrase `комп`, RU primary language, EN fallback, model paths, and LM Studio.
- Wake phrase text detection with Vosk-compatible grammar variants.
- Plugin manifests in TOML with aliases, regex slots, and action types.
- Scenario manifests in TOML with ordered steps, conditions, `ask` replies, and branching.
- System monitor commands for media pause/next/previous, scrolling, battery status, and optional battery/charging voice prompts.
- Multiple wake phrases through `wake_phrases`, while legacy `wake_grammar` still works.
- MP3/WAV/OGG sound playback actions via `play_sound` / `say_sound`.
- Offline cloned-voice text-to-speech through manually selected XTTS v2 or CosyVoice sidecars and the `say_text` scenario action.
- LM Studio fallback client using `POST /v1/chat/completions`.
- CPAL microphone capture behind the `audio-cpal` feature.
- Vosk STT adapter behind the `vosk-stt` feature.
- WAV loading helpers for fixture/manual tests.
- Local daemon API:
  - `GET /health`
  - `GET /events` SSE
  - `GET /events/ws` WebSocket
  - `POST /commands/reload`
  - `POST /listen/once`
  - `POST /listen/live` when built with `--features live-vosk`
  - `POST /listen/start` to start the always-on wake loop when built with `--features live-vosk`
  - `POST /listen/stop` to stop the always-on wake loop
  - `GET /config`
  - `POST /config`
  - `POST /lmstudio/test`
  - `GET /tts/status`, `POST /tts/install`, `POST /tts/start`, `POST /tts/stop`
  - `POST /tts/test`, `POST /tts/speak`
  - `GET /tts/voices`, `POST /tts/voices`, `DELETE /tts/voices/{id}`
  - `GET /models`
  - `GET /scenarios`, `GET /scenarios/{id}`
  - `POST /scenarios`, `PUT /scenarios/{id}`, `DELETE /scenarios/{id}`
  - `POST /scenarios/{id}/validate`
  - `POST /scenarios/{id}/dry-run`
  - `POST /scenarios/{id}/sounds`
  - `GET /apps`
  - `GET /logo`

Models are configured as local paths and are not bundled into this repository.

## Try It

One-command macOS prototype:

```bash
./scripts/run-prototype-macos.sh
```

It downloads native Vosk, downloads small RU/EN models, writes `komp.prototype.toml`, builds the daemon with `live-vosk`, starts the always-on wake loop, and prints logs in the terminal. Say `комп`, then speak a command; the recognized text and resolved intent will appear in the logs.

Prototype launch scripts check `origin/<current-branch>` before starting. If the local checkout is behind and tracked files are clean, KOMP fast-forwards itself with `git pull --ff-only`; user files such as `plugins.user`, local models, and untracked sounds are left alone. If tracked files have local edits, auto-update is skipped. Set `KOMP_NO_AUTO_UPDATE=1` to disable this check.

Windows prototype:

```powershell
.\scripts\run-prototype-windows.ps1
```

Ubuntu/Debian prototype:

```bash
./scripts/install-linux-deps.sh
./scripts/run-prototype-linux.sh
```

To verify Ubuntu/Debian build support without starting the microphone loop:

```bash
./scripts/verify-linux.sh
```

The same scripts are intended for regular Ubuntu and Debian desktop installs; CI runs the Linux build path on `ubuntu-latest`.

For Linux `set_volume` uses `wpctl` when available and falls back to `pactl`; install `wireplumber` or `pulseaudio-utils` if volume scenarios need it. Linux `hotkey` uses `xdotool`, which is usually X11-only and only needed for hotkey scenarios.

Manual commands:

```bash
cargo run -p erez-cli -- init-config komp.toml
cargo run -p erez-cli -- wake-test "комп, открой браузер"
cargo run -p erez-cli -- plugins-validate plugins.example
cargo run -p erez-cli -- scenarios-validate plugins.example
cargo run -p erez-cli -- scenario-run browser_quieter --plugins plugins.example --dry-run
cargo run -p erez-cli -- sound-test sounds/system/listening.mp3 --dry-run
cargo run -p erez-cli -- resolve "открой браузер" --plugins plugins.example --no-lmstudio
cargo run -p erez-cli -- wav-info ./command.wav
cargo run -p erez-cli -- whisper-wav ./command.wav --config komp.prototype.toml
cargo run -p erez-daemon
```

The daemon loads `komp.toml` from the current directory, then falls back to `erez.toml`; `KOMP_CONFIG` overrides both. `EREZ_CONFIG` still works for compatibility.

## Desktop Scenario Builder

The desktop scenario builder lives in `apps/komp-desktop`. It is a dark-blue minimal Tauri app for creating scenario folders without editing TOML by hand.

Run the daemon first:

```bash
cargo run -p erez-daemon
```

Then run the desktop shell:

```bash
cd apps/komp-desktop
npm install
npm run dev
```

The app talks to `http://127.0.0.1:3737`, lists system and user scenarios, and writes user scenarios into:

```text
plugins.user/scenarios/<scenario_id>/scenario.toml
```

Each scenario can store its own uploaded MP3/WAV/OGG files under:

```text
plugins.user/scenarios/<scenario_id>/sounds/
```

System scenarios from `plugins.example` are shown read-only. If `logo.png`, `logo.svg`, `komp-logo.png`, or another supported logo file is placed in the project root, the app displays it in the sidebar.

The app also has an `LM Studio` settings view. Use it to enable/disable fallback parsing, edit `base_url`, select a model returned by `POST /lmstudio/test`, and save the updated config back to the daemon.

## Dynamic Text To Speech

The `Голос` view offers a manual choice between XTTS v2 and CosyVoice. XTTS is the faster option for low-power computers; CosyVoice remains the heavier quality option. Providers never fall back to one another automatically. Each provider has an isolated Python environment under `vendor/xtts` or `vendor/cosyvoice`, and Docker is not used.

XTTS uses the CPML non-commercial model license. Accept it explicitly in the app before installation. KOMP then downloads the model, keeps it loaded in the local sidecar, and selects CUDA, tested MPS, or CPU according to the configured device. Use `Проверить скорость` to see first-chunk latency and real-time factor for the current computer.

Create a cloned voice by uploading a clean 3-15 second MP3/WAV/OGG sample. Its exact transcript is required by CosyVoice and optional for XTTS. Profiles are normalized to mono 16 kHz WAV and stored under `voices/<voice_id>/`; XTTS conditioning is cached next to the profile and regenerated when the sample changes. Generated phrases are separated by provider under `cache/tts/`. These directories are local user data and ignored by git.

Dynamic speech can be added to a scenario directly in the editor or TOML:

```toml
[[scenarios.steps]]
id = "answer"
action = { type = "say_text", text = "Заряд {{battery_percent}} процентов", voice = "komp", speed = 1.0, cache = true }
```

Questions can also be synthesized instead of using a recorded prompt:

```toml
[[scenarios.steps]]
id = "ask_browser"
action = { type = "ask", text = "Какой браузер открыть?", reply_slot = "browser" }
```

`ask` accepts either `text` or `sound`. Existing `play_sound`, `say_sound`, scenario sounds, and system MP3 files remain unchanged. While a TTS provider is generating or speaking, wake recognition is guarded from hearing KOMP itself; `комп стоп` cancels generation/playback and the remaining scenario steps.

Scenario HTTP actions can pass slots in the URL, headers, and JSON body, then save either the full response or one JSON field for later steps:

```toml
[[scenarios.steps]]
id = "load_value"
action = { type = "http_request", method = "GET", url = "https://api.example.com/items/{{item_id}}", headers = { Authorization = "Bearer {{token}}" }, response_slot = "price", json_path = "data.price", timeout_ms = 10000 }

[[scenarios.steps]]
id = "speak_value"
action = { type = "say_text", text = "Цена {{price}} рублей", speed = 1.0, cache = false }
```

The desktop editor exposes the same fields under `HTTP-запрос`. Built-in user scenarios `currency_converter` and `calculator` demonstrate result slots and mandatory dynamic replies. The currency converter uses the keyless [ExchangeRate-API](https://www.exchangerate-api.com) endpoint and therefore needs an internet connection; the calculator is fully local.

## Whisper Command STT

Vosk remains the wake recognizer and the fallback command recognizer. To try `whisper.cpp` for commands after wake, install/build `whisper-cli`, download a multilingual ggml model, then enable:

```toml
[whisper]
enabled = true
cli_path = "vendor/whisper.cpp/build/bin/whisper-cli"
model_path = "vendor/whisper.cpp/models/ggml-base.bin"
language = "ru"
timeout_ms = 8000
extra_args = ["-nt"]
```

If `whisper.cpp` is disabled, missing, times out, or returns an empty transcript, KOMP falls back to Vosk automatically.

To check brand/app names and measure latency on a recorded command:

```bash
cargo run -p erez-cli -- whisper-wav ./command.wav --config komp.prototype.toml
```

The JSON output includes `transcript.text` and `latency_ms`. In live mode, successful command recognition logs also include `stt_latency_ms`.

If the first syllable or word is clipped after wake, increase command pre-roll:

```toml
[audio]
command_preroll_ms = 300
```

Wake/listening sounds are played asynchronously, so KOMP starts recording the command immediately after wake detection instead of waiting for the mp3 to finish. Use short, quiet wake sounds or headphones if the microphone picks up the assistant sound.

## Sounds

Put MP3/WAV/OGG files into `sounds/` and reference them from config or scenarios.

Global assistant sounds are configured in `komp.toml`:

```toml
[sounds]
startup = "sounds/system/startup.mp3"
shutdown = "sounds/system/shutdown.mp3"
wake = "sounds/system/listening.mp3"
listening = "sounds/system/listening.mp3"
```

`startup` plays when KOMP starts, `shutdown` plays when KOMP stops, `wake` plays immediately after the wake phrase is detected, and `listening` plays right before command capture starts.

Optional system monitor voice prompts can be placed in `sounds/system/`:

- `power_connected.mp3|wav|ogg`
- `power_disconnected.mp3|wav|ogg`
- `battery_unavailable.mp3|wav|ogg`
- `battery_0_10.mp3|wav|ogg`
- `battery_10_20.mp3|wav|ogg`
- `battery_20_30.mp3|wav|ogg`
- `battery_30_40.mp3|wav|ogg`
- `battery_40_50.mp3|wav|ogg`
- `battery_50_60.mp3|wav|ogg`
- `battery_60_70.mp3|wav|ogg`
- `battery_70_80.mp3|wav|ogg`
- `battery_80_90.mp3|wav|ogg`
- `battery_90_100.mp3|wav|ogg`
- `battery_100.mp3|wav|ogg`

Battery prompts are intentionally bucketed: for example, 50-59% plays `battery_50_60.mp3`, 60-69% plays `battery_60_70.mp3`, and exactly 100% plays `battery_100.mp3`. Old files named `battery_gt_50.mp3` through `battery_gt_100.mp3` still work as fallback. KOMP plays charge level prompts when asked with phrases like `сколько зарядки`, and also after charger connect/disconnect events.

Then:

```bash
curl http://127.0.0.1:3737/health
curl -X POST http://127.0.0.1:3737/listen/once \
  -H 'content-type: application/json' \
  -d '{"transcript":"найди погоду в москве"}'
```

Dialog scenarios can be tested through `replies`:

```bash
curl -X POST http://127.0.0.1:3737/listen/once \
  -H 'content-type: application/json' \
  -d '{"transcript":"открой браузер какой","replies":["chrome"]}'
```

## Scenarios

Scenarios can live in their own folders under a plugin directory. KOMP loads TOML manifests recursively, so this works:

```text
plugins.example/
  scenarios/
    hello/
      scenario.toml
    restart/
      scenario.toml
```

A scenario manifest looks like:

```toml
[[scenarios]]
id = "browser_quieter"
aliases = ["включи браузер громкость ниже", "open browser lower volume"]
priority = 20

[[scenarios.steps]]
id = "ack"
action = { type = "play_sound", file = "sounds/system/listening.mp3" }
on_error = "open_browser"

[[scenarios.steps]]
id = "open_browser"
action = { type = "open_app", app = "Google Chrome" }

[[scenarios.steps]]
id = "lower_volume"
action = { type = "set_volume", delta = -15 }
```

Branching can use `when`, `on_success`, and `on_error`. Dialog steps use `ask` or `wait_for_reply` with a `reply_slot`, then later steps can branch on that slot.

Examples:

```toml
[[scenarios.steps]]
id = "only_macos"
when = { os = "macos" }
action = { type = "open_app", app = "Safari" }

[[scenarios.steps]]
id = "chrome_reply"
when = { slot = "browser", contains = "chrome" }
action = { type = "open_app", app = "Google Chrome" }

[[scenarios.steps]]
id = "after_success"
when = { previous_success = true }
action = { type = "play_sound", file = "sounds/success.mp3" }
```

## Live Vosk Mode

The one-command prototype above is preferred. For manual setup, configure `komp.toml` with local model directories:

```toml
[models]
ru_vosk_path = "/absolute/path/to/vosk-model-small-ru"
en_vosk_path = "/absolute/path/to/vosk-model-small-en-us"
```

Install native Vosk:

```bash
./scripts/setup-vosk-macos.sh
# or
./scripts/setup-vosk-linux.sh
```

Run the live daemon with autostart:

```bash
KOMP_CONFIG=komp.toml KOMP_AUTOSTART=1 ./scripts/run-daemon-live-macos.sh
# or
KOMP_CONFIG=komp.toml KOMP_AUTOSTART=1 ./scripts/run-daemon-live-linux.sh
```

Then trigger one microphone command window:

```bash
curl -X POST http://127.0.0.1:3737/listen/live
```

Or start the always-on wake loop:

```bash
curl -X POST http://127.0.0.1:3737/listen/start
curl -X POST http://127.0.0.1:3737/listen/stop
```

While a command or scenario is running, say `комп стоп` after the wake phrase to request cancellation. KOMP rejects new normal commands while busy, but accepts the stop command and cancels remaining scenario steps.

Transcribe a WAV without the daemon:

```bash
cargo run -p erez-cli --features vosk-stt -- transcribe-wav ./command.wav --config komp.toml --language ru
```

On macOS the app needs microphone permission. On Windows and Linux, the default input device must be available to CPAL. On Ubuntu/Debian, install ALSA development headers before building live audio: `sudo apt install libasound2-dev pkg-config`.
