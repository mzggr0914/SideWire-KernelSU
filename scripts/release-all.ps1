param([switch]$SetupLinux)

$ErrorActionPreference = "Stop"

Write-Host "==> Windows + Android release"
& (Join-Path $PSScriptRoot "release.ps1")
if ($LASTEXITCODE -ne 0) {
    throw "Windows/Android release failed with exit code $LASTEXITCODE"
}

Write-Host "==> Linux release"
if ($SetupLinux) {
    & (Join-Path $PSScriptRoot "release-linux.ps1") -Setup
} else {
    & (Join-Path $PSScriptRoot "release-linux.ps1")
}
if ($LASTEXITCODE -ne 0) {
    throw "Linux release failed with exit code $LASTEXITCODE"
}

Write-Host "==> All release artifacts ready"
