param(
    [switch]$SkipHost,
    [switch]$SkipAndroid,
    [switch]$SkipWebUi
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Invoke-Checked([scriptblock]$Command, [string]$Name) {
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

if (-not $SkipHost) {
    Write-Host "==> Building Windows CLI"
    Invoke-Checked { cargo build --release -p sidewire } "Windows CLI build"
}

if (-not $SkipWebUi) {
    Write-Host "==> Building KernelSU WebUI"
    Push-Location (Join-Path $root "webui")
    try {
        Invoke-Checked { pnpm.cmd install --frozen-lockfile } "pnpm install"
        Invoke-Checked { pnpm.cmd build } "WebUI build"
    } finally {
        Pop-Location
    }
    $webroot = Join-Path $root "module\webroot"
    if (Test-Path $webroot) {
        Remove-Item $webroot -Recurse -Force
    }
    Copy-Item (Join-Path $root "webui\dist") $webroot -Recurse
}

if (-not $SkipAndroid) {
    Write-Host "==> Building Android daemon"
    $ndkCandidates = @(
        $env:ANDROID_NDK_HOME,
        (Join-Path $env:LOCALAPPDATA "Android\Sdk\ndk\29.0.14206865")
    ) | Where-Object { $_ -and (Test-Path $_) }
    $ndk = $ndkCandidates | Select-Object -First 1
    if (-not $ndk) {
        throw "Android NDK not found. Set ANDROID_NDK_HOME or install NDK r29."
    }
    $env:ANDROID_NDK_HOME = $ndk
    $env:PATH = "$(Join-Path $env:USERPROFILE '.cargo\bin');$env:PATH"
    Invoke-Checked { cargo ndk -t arm64-v8a -P 23 build --release -p sidewired } "Android daemon build"

    $source = Join-Path $root "target\aarch64-linux-android\release\sidewired"
    $dest = Join-Path $root "module\bin\sidewired"
    Copy-Item $source $dest -Force
}

Write-Host "==> Build complete"
