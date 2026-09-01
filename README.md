# SideWire

ADB-independent native control bridge for rooted Android devices. SideWire 0.6.1 consists of a Rust desktop CLI/server, a Rust Android daemon packaged as a KernelSU module, and a shared framed protocol.

## Components

- `apps/sidewire`: Windows/Linux desktop CLI and device/control server
- `apps/sidewired`: Android daemon
- `crates/sidewire-protocol`: shared protocol
- `webui`: KernelSU WebUI source
- `module`: KernelSU module source files
- `scripts/build.ps1`: Windows CLI + WebUI + Android build
- `scripts/release.ps1`: Windows CLI + KernelSU ZIP release
- `scripts/setup-linux.sh`: install a user-local stable Rust toolchain on Linux
- `scripts/build-linux.sh`: native Linux CLI build
- `scripts/release-linux.sh`: native Linux release artifact
- `scripts/*-linux.ps1`: WSL wrappers for the Linux scripts
- `scripts/build-all.ps1` / `release-all.ps1`: Windows + Android + Linux in one command

Generated output (`target/`, `target-*`, `webui/node_modules/`, `webui/dist/`, `module/webroot/`, `module/bin/sidewired`, `dist/`) is ignored by Git.

## Windows / Android release

From PowerShell:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\release.ps1
```
This creates:

```text
dist/sidewire.exe
dist/SideWire-KernelSU-v0.6.1-arm64.zip
```

The packager verifies required module entries and rejects Windows-style `\` separators inside the ZIP so KernelSU can detect `webroot/index.html` correctly.

## Linux build and release

On Linux:

```bash
bash ./scripts/setup-linux.sh     # first time only
bash ./scripts/build-linux.sh
bash ./scripts/release-linux.sh
```

From Windows with WSL Ubuntu:

```powershell
powershell -File .\scripts\setup-linux.ps1
powershell -File .\scripts\build-linux.ps1
powershell -File .\scripts\release-linux.ps1
```

The Linux release is written as `dist/sidewire-v0.6.1-linux-<arch>`. To build every platform from Windows, use `powershell -File .\scripts\build-all.ps1` or `powershell -File .\scripts\release-all.ps1`. Add `-SetupLinux` on the first run.

## Desktop convenience config

SideWire stores optional desktop defaults in `%APPDATA%\SideWire\config.json` on Windows or `$XDG_CONFIG_HOME/sidewire/config.json` / `~/.config/sidewire/config.json` on Unix.

```powershell
.\dist\sidewire.exe config set connect 192.168.0.123:58321
.\dist\sidewire.exe config set run-as root
.\dist\sidewire.exe config show
.\dist\sidewire.exe config unset run-as
```

When `connect` is configured, plain `sidewire server` automatically maintains that inbound connection. When `run-as` is configured, commands that support `--as` use it when the flag is omitted. Explicit CLI flags still win.

## Connection modes

### Outbound (Android connects to PC)

Set the module WebUI to `Outbound`, enter the PC IP/port, start the desktop server, then use the normal CLI commands:

```powershell
.\dist\sidewire.exe server
.\dist\sidewire.exe devices
.\dist\sidewire.exe shell
```

### Inbound (PC connects to Android)

Set the module WebUI to `Inbound` and choose the listening port. Then point the desktop server at the phone:

```powershell
.\dist\sidewire.exe server --connect 192.168.0.123:58321
```

The server keeps the normal outbound listener active too and automatically retries the configured inbound endpoint after a disconnect. Once connected, `devices`, `shell`, `exec`, `push`, `pull`, forward/reverse and the other CLI commands work through the same local control port (`127.0.0.1:58322`).

For a single inbound device on the local LAN, the daemon also answers SideWire UDP discovery:

```powershell
.\dist\sidewire.exe discover
.\dist\sidewire.exe server --discover
```

`server --discover` rediscovers the device after disconnects/IP changes. This convenience path intentionally targets one discovered device; multi-device discovery/selection is not part of this release.

## Shell and Tab completion
On Windows, the default shell keeps cooked console editing for low-latency local typing. SideWire completes against the live Android PTY context:

- command position: complete executable names from the remote `$PATH`
- path arguments: complete files/directories from the current remote working directory
- `cd`: show directory candidates only
- `~` / `~/...`: expand against the remote `HOME` when available
- first Tab: extend to the longest common prefix (or complete the only match)
- second Tab at the same completed line/cursor: print matching candidates in columns and redraw the prompt/input line
- candidate display is capped at 256 entries and reports how many additional matches exist

`--raw` forwards terminal input directly to the Android PTY.

On Linux/Unix, the default shell uses the raw PTY path so the remote shell provides its native line editor, history and Tab behavior.

```powershell
.\dist\sidewire.exe shell
.\dist\sidewire.exe shell --as root
.\dist\sidewire.exe shell --raw
```

## Developer convenience commands

```powershell
.\dist\sidewire.exe doctor
.\dist\sidewire.exe wait-for-device --timeout 30
```

`doctor` checks the local control server, connected device, protocol round trip, Android model/version, shell identity and root execution. `wait-for-device` also tolerates the local server being temporarily unavailable and can be used after reboot; use `--timeout 0` to wait indefinitely.

`push` and `pull` automatically recurse when the source path is a directory:

```powershell
.\dist\sidewire.exe push .\build /data/local/tmp/build
.\dist\sidewire.exe pull /sdcard/MyFolder .\MyFolder
```

Recursive push preserves the directory tree and rejects local symlinks rather than following them unexpectedly. Recursive pull restores regular files/directories reported by Android `find`.

## Common commands

```powershell
.\dist\sidewire.exe exec id
.\dist\sidewire.exe exec --as root id
.\dist\sidewire.exe push .\local.bin /data/local/tmp/local.bin
.\dist\sidewire.exe pull /sdcard/screenshot.png .\screenshot.png
.\dist\sidewire.exe logcat
.\dist\sidewire.exe packages
.\dist\sidewire.exe app start com.example.app
```
## Development checks

Windows:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\release.ps1
```

Linux/WSL:

```bash
cargo fmt --all -- --check
CARGO_TARGET_DIR=target-linux cargo clippy --workspace --all-targets -- -D warnings
CARGO_TARGET_DIR=target-linux cargo test --workspace
bash ./scripts/release-linux.sh
```

The shared wire protocol is version 7. Protocol changes require rebuilding both the desktop CLI and Android module; mismatched protocol versions are rejected during frame decoding.

## Security

SideWire currently does not provide transport authentication or encryption. The Android daemon can execute privileged operations, so do not expose either connection mode to untrusted networks. Keep SideWire on a trusted LAN/VPN or bind/listen more narrowly until authentication is implemented.
