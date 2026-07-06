$ErrorActionPreference = "Stop"

$RootDir = Resolve-Path (Join-Path $PSScriptRoot "..")
$VendorDir = Join-Path $RootDir "vendor\vosk"
$ModelsDir = Join-Path $VendorDir "models"
$ConfigPath = Join-Path $RootDir "komp.prototype.toml"
$RuModelVersion = if ($env:EREZ_RU_MODEL_VERSION) { $env:EREZ_RU_MODEL_VERSION } else { "0.22" }
$EnModelVersion = if ($env:EREZ_EN_MODEL_VERSION) { $env:EREZ_EN_MODEL_VERSION } else { "0.15" }

function Download-Model($Lang, $Url) {
    $Dest = Join-Path $ModelsDir $Lang
    if ((Test-Path (Join-Path $Dest "am\final.mdl")) -or (Test-Path (Join-Path $Dest "conf\model.conf"))) {
        Write-Host "Model $Lang already installed at $Dest"
        return
    }

    $Tmp = Join-Path $RootDir "target\vosk-model-$Lang"
    $Archive = Join-Path $Tmp "model.zip"
    New-Item -ItemType Directory -Force -Path $Tmp | Out-Null
    New-Item -ItemType Directory -Force -Path $ModelsDir | Out-Null
    Write-Host "Downloading $Lang model from $Url"
    Invoke-WebRequest -Uri $Url -OutFile $Archive
    $ExtractDir = Join-Path $Tmp "extract"
    if (Test-Path $ExtractDir) { Remove-Item -Recurse -Force $ExtractDir }
    New-Item -ItemType Directory -Force -Path $ExtractDir | Out-Null
    Expand-Archive -Path $Archive -DestinationPath $ExtractDir
    if (Test-Path $Dest) { Remove-Item -Recurse -Force $Dest }
    $Extracted = Get-ChildItem -Path $ExtractDir -Directory | Select-Object -First 1
    if (-not $Extracted) { throw "Model archive did not contain a model directory" }
    Move-Item $Extracted.FullName $Dest
    Write-Host "Installed $Lang model into $Dest"
}

if (-not (Test-Path (Join-Path $VendorDir "lib\vosk.dll"))) {
    & (Join-Path $RootDir "scripts\setup-vosk-windows.ps1")
} else {
    Write-Host "Native Vosk library already installed"
}

Download-Model "ru" "https://alphacephei.com/vosk/models/vosk-model-small-ru-$RuModelVersion.zip"
Download-Model "en" "https://alphacephei.com/vosk/models/vosk-model-small-en-us-$EnModelVersion.zip"

@"
wake_phrase = "комп"
wake_phrases = ["комп", "компьютер"]
wake_grammar = ["комп", "компьютер"]
primary_language = "ru"
english_fallback = true
plugin_dirs = ["plugins.example"]

[models]
ru_vosk_path = "$($ModelsDir -replace '\\','/')/ru"
en_vosk_path = "$($ModelsDir -replace '\\','/')/en"

[lmstudio]
enabled = false
base_url = "http://localhost:1234/v1"
model = "local-model"
timeout_ms = 2500
min_confidence = 0.55

[whisper]
enabled = false
# cli_path = "vendor/whisper.cpp/build/bin/Release/whisper-cli.exe"
# model_path = "vendor/whisper.cpp/models/ggml-base.bin"
language = "ru"
timeout_ms = 8000
extra_args = ["-nt"]

[audio]
sample_rate_hz = 16000
command_timeout_ms = 7000
end_silence_ms = 900
command_preroll_ms = 300

[sounds]
startup = "sounds/system/startup.mp3"
shutdown = "sounds/system/shutdown.mp3"
wake = "sounds/system/listening.mp3"
listening = "sounds/system/listening.mp3"
"@ | Set-Content -Encoding UTF8 $ConfigPath

$env:PATH = "$(Join-Path $VendorDir 'lib');$env:PATH"
$env:KOMP_CONFIG = $ConfigPath
$env:EREZ_CONFIG = $ConfigPath
$env:KOMP_AUTOSTART = "1"
$env:EREZ_AUTOSTART = "1"
$env:RUST_LOG = if ($env:KOMP_RUST_LOG) { $env:KOMP_RUST_LOG } elseif ($env:EREZ_RUST_LOG) { $env:EREZ_RUST_LOG } else { "komp_daemon=info,erez_daemon=info,erez_core=info" }

Write-Host ""
Write-Host "Starting KOMP prototype."
Write-Host "Say: комп ... then speak a command. Logs will appear below."
Write-Host "Config: $ConfigPath"
Write-Host ""

cargo run -p erez-daemon --features live-vosk
