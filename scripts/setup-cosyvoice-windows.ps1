$ErrorActionPreference = "Stop"

$RootDir = Resolve-Path (Join-Path $PSScriptRoot "..")
$RuntimeDir = Join-Path $RootDir "vendor\cosyvoice"
$SourceDir = Join-Path $RuntimeDir "source"
$VenvDir = Join-Path $RuntimeDir ".venv"
$Python = if ($env:KOMP_COSYVOICE_PYTHON) { $env:KOMP_COSYVOICE_PYTHON } elseif (Get-Command py -ErrorAction SilentlyContinue) { "py" } else { "python" }

if ($Python -ne "py") {
    $PythonVersion = & $Python -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')"
    if ($PythonVersion -ne "3.10") { throw "CosyVoice requires Python 3.10; found $PythonVersion. Set KOMP_COSYVOICE_PYTHON." }
}

New-Item -ItemType Directory -Force -Path (Join-Path $RuntimeDir "models") | Out-Null
if (-not (Test-Path (Join-Path $SourceDir ".git"))) {
    Write-Host "Cloning CosyVoice..."
    git clone --recursive https://github.com/FunAudioLLM/CosyVoice.git $SourceDir
} else {
    Write-Host "CosyVoice source already installed"
    git -C $SourceDir submodule update --init --recursive
}

$VenvPython = Join-Path $VenvDir "Scripts\python.exe"
if (-not (Test-Path $VenvPython)) {
    if ($Python -eq "py") { & py -3.10 -m venv $VenvDir } else { & $Python -m venv $VenvDir }
}
& $VenvPython -m pip install --upgrade "pip<25" "setuptools<81" wheel packaging
& $VenvPython -m pip install --no-build-isolation openai-whisper==20231117
& $VenvPython -m pip install -r (Join-Path $SourceDir "requirements.txt")
& $VenvPython -m pip install huggingface_hub

$ModelDir = Join-Path $RuntimeDir "models\Fun-CosyVoice3-0.5B"
if (-not (Test-Path $ModelDir)) {
    Write-Host "Downloading Fun-CosyVoice3-0.5B-2512..."
    $Download = "from huggingface_hub import snapshot_download; snapshot_download('FunAudioLLM/Fun-CosyVoice3-0.5B-2512', local_dir=r'$ModelDir')"
    & $VenvPython -c $Download
} else {
    Write-Host "CosyVoice model already installed"
}

Write-Host "CosyVoice installation completed"
