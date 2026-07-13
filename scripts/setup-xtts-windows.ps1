$ErrorActionPreference = "Stop"

if ($env:KOMP_XTTS_ACCEPT_CPML -ne "1") {
    throw "XTTS v2 uses the CPML non-commercial license. Accept it in KOMP first."
}

$RootDir = Resolve-Path (Join-Path $PSScriptRoot "..")
$RuntimeDir = Join-Path $RootDir "vendor\xtts"
$VenvDir = Join-Path $RuntimeDir ".venv"
$Model = if ($env:KOMP_XTTS_MODEL) { $env:KOMP_XTTS_MODEL } else { "tts_models/multilingual/multi-dataset/xtts_v2" }
$Python = if ($env:KOMP_XTTS_PYTHON) { $env:KOMP_XTTS_PYTHON } elseif (Get-Command py -ErrorAction SilentlyContinue) { "py" } else { "python" }

New-Item -ItemType Directory -Force -Path $RuntimeDir | Out-Null
$VenvPython = Join-Path $VenvDir "Scripts\python.exe"
if (-not (Test-Path $VenvPython)) {
    if ($Python -eq "py") {
        $Created = $false
        foreach ($Version in @("3.13", "3.12", "3.11", "3.10")) {
            & py "-$Version" -m venv $VenvDir 2>$null
            if ($LASTEXITCODE -eq 0) { $Created = $true; break }
        }
        if (-not $Created) { throw "XTTS v2 requires Python 3.10-3.14." }
    } else { & $Python -m venv $VenvDir }
}
& $VenvPython -m pip install --upgrade "pip<26" "setuptools<81" wheel
& $VenvPython -m pip install torch torchaudio
& $VenvPython -m pip install coqui-tts "transformers<5.1" fastapi uvicorn numpy soundfile
$env:COQUI_TOS_AGREED = "1"
$env:TTS_HOME = Join-Path $RuntimeDir "models"
$Download = "from pathlib import Path; from TTS.api import TTS; TTS(model_name='$Model', progress_bar=True); Path(r'$RuntimeDir\model-installed').write_text('$Model', encoding='utf-8')"
& $VenvPython -c $Download
Write-Host "XTTS v2 installation completed"
