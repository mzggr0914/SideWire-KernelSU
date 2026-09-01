param([switch]$Setup)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

if ($Setup) {
    & (Join-Path $PSScriptRoot "setup-linux.ps1")
}

$drive = $root.Substring(0, 1).ToLowerInvariant()
$rest = $root.Substring(2).Replace('\', '/')
$linuxRoot = "/mnt/$drive$rest"
& wsl.exe bash -lc "cd '$linuxRoot' && bash ./scripts/build-linux.sh"
if ($LASTEXITCODE -ne 0) {
    throw "Linux build failed with exit code $LASTEXITCODE"
}
