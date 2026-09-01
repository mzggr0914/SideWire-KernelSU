# SideWire

ADB-independent native control bridge for rooted Android devices, built around a Rust desktop CLI/server and a Rust Android daemon packaged as a KernelSU module.

## Components

- `apps/sidewire`: Windows desktop CLI and device/control server
- `apps/sidewired`: Android daemon
- `crates/sidewire-protocol`: shared framed protocol
- `webui`: KernelSU WebUI source
- `module`: KernelSU module source files
- `scripts/build.ps1`: builds host, WebUI, and Android daemon
- `scripts/release.ps1`: builds and packages release artifacts into `dist/`

Generated output (`target/`, `webui/node_modules/`, `webui/dist/`, `module/webroot/`, `module/bin/sidewired`, `dist/`) is intentionally ignored by Git.

## Build and release

From PowerShell:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\release.ps1
```

The release script builds the Windows CLI, WebUI, and arm64 Android daemon, then creates:

```text
dist/sidewire.exe
dist/SideWire-KernelSU-v<version>-arm64.zip
```

The packager verifies that required module entries exist and rejects Windows-style `\` path separators inside the ZIP so KernelSU can always detect `webroot/index.html` correctly.

## Quick start

1. Install the generated KernelSU ZIP on the Android device.
2. Open the SideWire module WebUI and configure mode, host, port, device name, and autostart as needed.
3. Start the desktop server:

```powershell
.\dist\sidewire.exe server
```

4. Confirm the device is connected:

```powershell
.\dist\sidewire.exe devices
```

5. Open an interactive shell:

```powershell
.\dist\sidewire.exe shell
.\dist\sidewire.exe shell --as root
```

The default shell mode keeps Windows cooked line editing for responsive local typing. Tab is handled as an intermediate completion request and completes remote Android files/directories using the active PTY shell's current working directory.

`--raw` is still available when every keystroke should be forwarded directly to the Android PTY:

```powershell
.\dist\sidewire.exe shell --raw
```

## Common commands

```powershell
.\dist\sidewire.exe exec id
.\dist\sidewire.exe exec --as root id
.\dist\sidewire.exe push .\local.bin /data/local/tmp/local.bin
.\dist\sidewire.exe pull /sdcard/screenshot.png .\screenshot.png
.\dist\sidewire.exe logcat
.\dist\sidewire.exe logcat --clear
.\dist\sidewire.exe packages
.\dist\sidewire.exe app start com.example.app
```

Use `-s <device-name>` when more than one device is connected. The desktop control listener defaults to `127.0.0.1:58322`; the device listener defaults to `0.0.0.0:58321`.

## Development checks

Before committing protocol or PTY changes, run:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\release.ps1
```

The shared wire protocol is currently version 5. Protocol changes require rebuilding both `sidewire.exe` and the Android KernelSU module; mismatched protocol versions are rejected during connection setup.

## Repository layout

```text
apps/sidewire/        desktop CLI/server
apps/sidewired/       Android daemon
crates/sidewire-protocol/
module/               KernelSU module source
webui/                KernelSU WebUI source
scripts/              build/release automation
dist/                 generated release artifacts
```

## Security

SideWire currently does not provide transport authentication or encryption. The Android daemon can execute privileged operations, so do not expose the device listener to untrusted networks. Keep it on a trusted LAN/VPN or bind it more narrowly until authentication is implemented.
