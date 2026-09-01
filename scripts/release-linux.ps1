param(
    [switch]$Setup,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

if ($Setup) {
    & (Join-Path $PSScriptRoot "setup-linux.ps1")
}

$drive = $root.Substring(0, 1).ToLowerInvariant()
$rest = $root.Substring(2).Replace('\', '/')
$linuxRoot = "/mnt/$drive$rest"
$skip = if ($SkipBuild) { " --skip-build" } else { "" }

& wsl.exe bash -lc "cd '$linuxRoot' && bash ./scripts/release-linux.sh$skip"
if ($LASTEXITCODE -ne 0) {
    throw "Linux release failed with exit code $LASTEXITCODE"
}
