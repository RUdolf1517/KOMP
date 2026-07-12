$ErrorActionPreference = "Stop"

$Version = if ($env:VOSK_VERSION) { $env:VOSK_VERSION } else { "0.3.45" }
$RootDir = Resolve-Path (Join-Path $PSScriptRoot "..")
$VendorDir = Join-Path $RootDir "vendor\vosk"
$TmpDir = Join-Path $RootDir "target\vosk-setup"
$Archive = Join-Path $TmpDir "vosk-win64-$Version.zip"
$Url = "https://github.com/alphacep/vosk-api/releases/download/v$Version/vosk-win64-$Version.zip"

New-Item -ItemType Directory -Force -Path (Join-Path $VendorDir "lib") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $VendorDir "include") | Out-Null
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null

Write-Host "Downloading $Url"
Invoke-WebRequest -Uri $Url -OutFile $Archive
$ExtractDir = Join-Path $TmpDir "extract"
if (Test-Path $ExtractDir) { Remove-Item -Recurse -Force $ExtractDir }
New-Item -ItemType Directory -Force -Path $ExtractDir | Out-Null
Expand-Archive -Path $Archive -DestinationPath $ExtractDir

$Dll = Get-ChildItem -Path $ExtractDir -Recurse -Filter "vosk.dll" | Select-Object -First 1
$Lib = Get-ChildItem -Path $ExtractDir -Recurse -Include "libvosk.lib","vosk.lib" | Select-Object -First 1
$Header = Get-ChildItem -Path $ExtractDir -Recurse -Filter "vosk_api.h" | Select-Object -First 1
if (-not $Dll) { throw "vosk.dll was not found in archive" }

Copy-Item $Dll.FullName (Join-Path $VendorDir "lib\vosk.dll") -Force
if ($Lib) {
    Copy-Item $Lib.FullName (Join-Path $VendorDir "lib\libvosk.lib") -Force
    Copy-Item $Lib.FullName (Join-Path $VendorDir "lib\vosk.lib") -Force
} else {
    Write-Warning "Vosk import library was not found. Rust linking may fail until libvosk.lib is present."
}
if ($Header) { Copy-Item $Header.FullName (Join-Path $VendorDir "include\vosk_api.h") -Force }

Write-Host "Installed native Vosk library into $(Join-Path $VendorDir 'lib')"
Write-Host "For runtime, add that directory to PATH."
