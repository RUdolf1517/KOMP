$ErrorActionPreference = "Stop"

$RootDir = Resolve-Path (Join-Path $PSScriptRoot "..")
$VendorDir = Join-Path $RootDir "vendor\vosk"
$ModelsDir = Join-Path $VendorDir "models"
$ConfigPath = Join-Path $RootDir "komp.prototype.toml"
$RuModelVersion = if ($env:EREZ_RU_MODEL_VERSION) { $env:EREZ_RU_MODEL_VERSION } else { "0.22" }
$EnModelVersion = if ($env:EREZ_EN_MODEL_VERSION) { $env:EREZ_EN_MODEL_VERSION } else { "0.15" }

function Ensure-Rust {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "Required command not found: cargo. Install Rust from https://rustup.rs/"
    }
    $VersionText = (& cargo --version)
    $Version = ($VersionText -replace '^cargo\s+', '') -replace '\s+.*$', ''
    $Parts = $Version.Split('.')
    $Major = [int]$Parts[0]
    $Minor = [int]$Parts[1]
    if ($Major -gt 1 -or ($Major -eq 1 -and $Minor -ge 85)) {
        return
    }
    Write-Host "Cargo $Version is too old for KOMP dependencies. Rust/Cargo 1.85+ is required."
    if (Get-Command rustup -ErrorAction SilentlyContinue) {
        Write-Host "Updating Rust stable toolchain with rustup..."
        rustup toolchain install stable
        rustup default stable
        return
    }
    throw "Install a current Rust toolchain from https://rustup.rs/"
}

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

Ensure-Rust

& (Join-Path $RootDir "scripts\auto-update-git-windows.ps1")

if (-not (Test-Path (Join-Path $VendorDir "lib\vosk.dll")) -or -not (Test-Path (Join-Path $VendorDir "lib\libvosk.lib"))) {
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
command_timeout_ms = 10000
end_silence_ms = 1200
command_preroll_ms = 300

[sounds]
startup = "sounds/system/startup.mp3"
shutdown = "sounds/system/shutdown.mp3"
wake = "sounds/system/listening.mp3"
listening = "sounds/system/listening.mp3"
"@ | Set-Content -Encoding UTF8 $ConfigPath

$env:PATH = "$(Join-Path $VendorDir 'lib');$env:PATH"
$env:LIB = "$(Join-Path $VendorDir 'lib');$env:LIB"
$env:RUSTFLAGS = "-L native=$(Join-Path $VendorDir 'lib') $env:RUSTFLAGS"
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
