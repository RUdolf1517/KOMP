$ErrorActionPreference = "Stop"

$RootDir = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $RootDir

if ($env:KOMP_NO_AUTO_UPDATE -eq "1") {
    Write-Host "Auto-update disabled by KOMP_NO_AUTO_UPDATE=1"
    exit 0
}

if (-not (Get-Command git -ErrorAction SilentlyContinue) -or -not (Test-Path (Join-Path $RootDir ".git"))) {
    exit 0
}

$Branch = (& git branch --show-current 2>$null).Trim()
if (-not $Branch) {
    Write-Host "Auto-update skipped: detached HEAD"
    exit 0
}

& git remote get-url origin *> $null
if ($LASTEXITCODE -ne 0) {
    Write-Host "Auto-update skipped: origin remote is not configured"
    exit 0
}

& git diff --quiet
$DirtyWorktree = $LASTEXITCODE -ne 0
& git diff --cached --quiet
$DirtyIndex = $LASTEXITCODE -ne 0
if ($DirtyWorktree -or $DirtyIndex) {
    Write-Host "Auto-update skipped: tracked files have local changes"
    exit 0
}

Write-Host "Checking for KOMP updates on origin/$Branch..."
& git fetch --quiet origin $Branch
if ($LASTEXITCODE -ne 0) {
    Write-Host "Auto-update skipped: git fetch failed"
    exit 0
}

$LocalRev = (& git rev-parse HEAD).Trim()
$RemoteRev = (& git rev-parse "origin/$Branch").Trim()
$BaseRev = (& git merge-base HEAD "origin/$Branch").Trim()

if ($LocalRev -eq $RemoteRev) {
    Write-Host "KOMP is up to date."
} elseif ($LocalRev -eq $BaseRev) {
    Write-Host "Updating KOMP to $RemoteRev..."
    & git pull --ff-only origin $Branch
    if ($LASTEXITCODE -ne 0) { throw "git pull failed" }
    Write-Host "KOMP updated. Cargo will rebuild if needed."
} else {
    Write-Host "Auto-update skipped: local branch diverged from origin/$Branch"
}
