param([switch]$SkipBuild)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$cargoToml = Get-Content (Join-Path $root "Cargo.toml") -Raw
$versionMatch = [regex]::Match($cargoToml, '(?m)^version = "([^"]+)"')
if (-not $versionMatch.Success) {
    throw "Could not read workspace version from Cargo.toml"
}
$version = $versionMatch.Groups[1].Value

$moduleProp = Get-Content (Join-Path $root "module\module.prop") -Raw
if ($moduleProp -notmatch "(?m)^version=$([regex]::Escape($version))$") {
    throw "module/module.prop version does not match Cargo.toml ($version)"
}
$customize = Get-Content (Join-Path $root "module\customize.sh") -Raw
if ($customize -notmatch "SideWire $([regex]::Escape($version))") {
    throw "module/customize.sh version does not match Cargo.toml ($version)"
}
$webUiPackage = Get-Content (Join-Path $root "webui\package.json") -Raw | ConvertFrom-Json
if ($webUiPackage.version -ne $version) {
    throw "webui/package.json version does not match Cargo.toml ($version)"
}

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot "build.ps1")
    if ($LASTEXITCODE -ne 0) {
        throw "build.ps1 failed with exit code $LASTEXITCODE"
    }
}

$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Path $dist -Force | Out-Null
Copy-Item (Join-Path $root "target\release\sidewire.exe") (Join-Path $dist "sidewire.exe") -Force
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$moduleRoot = Join-Path $root "module"
$zipPath = Join-Path $dist "SideWire-KernelSU-v$version-arm64.zip"
if (Test-Path $zipPath) {
    Remove-Item $zipPath -Force
}

$archive = [System.IO.Compression.ZipFile]::Open(
    $zipPath,
    [System.IO.Compression.ZipArchiveMode]::Create
)
try {
    Get-ChildItem $moduleRoot -Recurse -File | ForEach-Object {
        $entryName = $_.FullName.Substring($moduleRoot.Length).TrimStart([char[]]@('\', '/')).Replace('\', '/')
        [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $archive,
            $_.FullName,
            $entryName,
            [System.IO.Compression.CompressionLevel]::Optimal
        ) | Out-Null
    }
} finally {
    $archive.Dispose()
}

$check = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
try {
    $entries = @($check.Entries | ForEach-Object FullName)
    if ($entries | Where-Object { $_ -match '\\' }) {
        throw "Release ZIP contains Windows-style path separators"
    }
    foreach ($required in @('module.prop', 'webroot/index.html', 'bin/sidewired', 'bin/sidewirectl')) {
        if ($entries -notcontains $required) {
            throw "Release ZIP is missing required entry: $required"
        }
    }
} finally {
    $check.Dispose()
}

Write-Host "==> Release artifacts"
Get-Item (Join-Path $dist "sidewire.exe"), $zipPath | Select-Object Name, Length
Get-FileHash (Join-Path $dist "sidewire.exe"), $zipPath -Algorithm SHA256 | Select-Object Path, Hash
