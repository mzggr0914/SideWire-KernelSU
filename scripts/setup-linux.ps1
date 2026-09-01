$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$drive = $root.Substring(0, 1).ToLowerInvariant()
$rest = $root.Substring(2).Replace('\', '/')
$linuxRoot = "/mnt/$drive$rest"
Write-Host "==> Preparing WSL Linux toolchain"
& wsl.exe bash -lc "cd '$linuxRoot' && bash ./scripts/setup-linux.sh"
if ($LASTEXITCODE -ne 0) {
    throw "Linux toolchain setup failed with exit code $LASTEXITCODE"
}
