param([switch]$SetupLinux)

$ErrorActionPreference = "Stop"

Write-Host "==> Windows + WebUI + Android build"
& (Join-Path $PSScriptRoot "build.ps1")
if ($LASTEXITCODE -ne 0) {
    throw "Windows/Android build failed with exit code $LASTEXITCODE"
}

Write-Host "==> Linux build"
if ($SetupLinux) {
    & (Join-Path $PSScriptRoot "build-linux.ps1") -Setup
} else {
    & (Join-Path $PSScriptRoot "build-linux.ps1")
}
if ($LASTEXITCODE -ne 0) {
    throw "Linux build failed with exit code $LASTEXITCODE"
}

Write-Host "==> All builds complete"
