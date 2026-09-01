$ErrorActionPreference = "Stop"

$project = Split-Path -Parent $MyInvocation.MyCommand.Path
$ndk = Join-Path $env:LOCALAPPDATA "Android\Sdk\ndk\29.0.14206865"
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"

if (-not (Test-Path $ndk)) {
    throw "Android NDK r29 not found at $ndk"
}

$env:ANDROID_NDK_HOME = $ndk
$env:PATH = "$cargoBin;$env:PATH"
Set-Location $project

cargo ndk -t arm64-v8a -P 23 build --release -p sidewired

$source = Join-Path $project "target\aarch64-linux-android\release\sidewired"
$dest = Join-Path $project "module\bin\sidewired"
Copy-Item $source $dest -Force

Write-Host "Built: $dest"
