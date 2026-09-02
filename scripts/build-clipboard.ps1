$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$version = "9.4.17"
$expectedSha256 = "2100511344497F041644A4D63FB7BE8A516CE9BACE30B7D17AB27CC93A0E58D4"
$toolDir = Join-Path $root "target\tools"
$r8 = Join-Path $toolDir "r8-$version.jar"
$source = Join-Path $root "android\clipboard\ClipboardHelper.java"
$classes = Join-Path $root "target\clipboard-classes"
$dexOut = Join-Path $root "target\clipboard-dex"
$dest = Join-Path $root "module\bin\sidewire-clipboard.jar"
$javac = (Get-Command javac -ErrorAction Stop).Source
$jdkBin = Split-Path -Parent $javac
$java = Join-Path $jdkBin "java.exe"
$jar = Join-Path $jdkBin "jar.exe"

New-Item -ItemType Directory -Force $toolDir | Out-Null
if (-not (Test-Path $r8)) {
    Write-Host "==> Downloading R8/D8 $version"
    $url = "https://dl.google.com/dl/android/maven2/com/android/tools/r8/$version/r8-$version.jar"
    Invoke-WebRequest -UseBasicParsing $url -OutFile $r8
}
$actualSha256 = (Get-FileHash $r8 -Algorithm SHA256).Hash
if ($actualSha256 -ne $expectedSha256) { throw "R8/D8 checksum mismatch" }
Remove-Item $classes,$dexOut -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $classes,$dexOut | Out-Null

& $javac -encoding UTF-8 -Xlint:-options -source 8 -target 8 -d $classes $source
if ($LASTEXITCODE -ne 0) { throw "javac failed" }
& $java -cp $r8 com.android.tools.r8.D8 --release --min-api 23 --output $dexOut (Join-Path $classes "com\sidewire\ClipboardHelper.class")
if ($LASTEXITCODE -ne 0) { throw "D8 failed" }
Remove-Item $dest -Force -ErrorAction SilentlyContinue
& $jar --create --file $dest -C $dexOut classes.dex
if ($LASTEXITCODE -ne 0) { throw "clipboard helper JAR packaging failed" }
Write-Host "==> Clipboard helper ready: $dest"
