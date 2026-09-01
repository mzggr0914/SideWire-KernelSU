# SideWire

ADB-independent native control bridge for rooted Android devices. SideWire 0.8.0 consists of a Rust desktop CLI/server, a Rust Android daemon packaged as a KernelSU module, and a shared framed protocol.

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
dist/SideWire-KernelSU-v0.8.0-arm64.zip
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

The Linux release is written as `dist/sidewire-v0.8.0-linux-<arch>`. To build every platform from Windows, use `powershell -File .\scripts\build-all.ps1` or `powershell -File .\scripts\release-all.ps1`. Add `-SetupLinux` on the first run.

## Desktop convenience config

SideWire stores optional desktop defaults in `%APPDATA%\SideWire\config.json` on Windows or `$XDG_CONFIG_HOME/sidewire/config.json` / `~/.config/sidewire/config.json` on Unix.

```powershell
.\dist\sidewire.exe config set connect 192.168.0.123:58321
.\dist\sidewire.exe config set default-device a1b2c3d4
.\dist\sidewire.exe config set run-as root
.\dist\sidewire.exe config show
.\dist\sidewire.exe config unset default-device
```

When `connect` is configured, plain `sidewire server` maintains that explicit inbound endpoint. `default-device` accepts a unique device name or device-ID prefix and is used whenever `-s` is omitted. `run-as` provides the default identity for commands that support `--as`. Explicit CLI flags always win.

## Security and pairing

SideWire 0.8.0 uses authenticated, encrypted connections by default. Pair each PC once from the Android WebUI: start SideWire, press **Generate pairing PIN**, then enter the six-digit PIN on the PC.

```powershell
.\dist\sidewire.exe pair 192.168.0.123
# In inbound mode, when exactly one compatible device is discoverable:
.\dist\sidewire.exe pair --discover
.\dist\sidewire.exe paired
```

The PIN is valid for 60 seconds and is only used for SPAKE2 pairing. A successful pairing exchanges a random 256-bit long-term secret; later connections authenticate and encrypt automatically without asking for the PIN again. The desktop stores its host identity and paired-device secrets in `trust.json` next to the normal SideWire config. Android stores paired-host secrets under the module's private `config/paired_hosts` directory.

Normal SideWire frames use a Noise PSK session (`25519` + `ChaChaPoly` + `BLAKE2s`) after authentication. Exec, PTY, file transfer, and secure forward/reverse proxy traffic are protected. UDP discovery is only locator metadata and is not itself trusted; the following TCP security handshake authenticates a paired peer.

To revoke trust, remove the PC from the Android WebUI. The desktop can remove its local device trust with:

```powershell
.\dist\sidewire.exe unpair <device-name-or-id-prefix>
```

### Insecure mode

For an intentionally trusted local network, authentication and encryption can be disabled. On Android, select **Insecure** in the WebUI; SideWire shows a warning dialog and does not save the change until you explicitly confirm it. Restart the daemon after changing security mode. On the PC, insecure mode must also be selected explicitly:

```powershell
.\dist\sidewire.exe server --insecure
```

Both ends must choose the same mode. A secure endpoint never falls back automatically to plaintext, so a secure/insecure mismatch is rejected. Insecure mode permits unauthenticated plaintext control and should only be used on a network you fully trust.

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

The server keeps the normal outbound listener active too and automatically retries configured inbound endpoints after a disconnect. Explicit `--connect` may be repeated, and it can be combined with `--discover`.

Inbound daemons answer SideWire UDP discovery with their persistent device ID:

```powershell
.\dist\sidewire.exe discover
.\dist\sidewire.exe server --discover
```

`discover` lists every inbound SideWire device that replies during the discovery window. `server --discover` maintains an independent connector for every discovered device, tracks endpoint/IP changes by device ID, and reconnects each device independently.

Every module installation gets a persistent 128-bit SideWire device ID. The host registry is keyed by that ID rather than the display name, so devices with identical names remain distinct. If the same ID appears through more than one connection path, the already-healthy session is kept and the duplicate connection is rejected.

## Device selection and multi-device commands

`sidewire devices` shows the short ID, display name, connection mode, security mode and peer. Commands accepting `-s` resolve a unique name or an ID prefix. If names collide, use the displayed ID prefix.

```powershell
.\dist\sidewire.exe shell -s a1b2c3d4
.\dist\sidewire.exe doctor -s a1b2c3d4
.\dist\sidewire.exe wait-for-device -s a1b2c3d4 --timeout 30
.\dist\sidewire.exe exec --all getprop ro.product.model
.\dist\sidewire.exe push --all --as root .\build /data/local/tmp/build
```

`exec --all` and `push --all` run per-device work concurrently. `--all` cannot be combined with `-s`. Without `-s`, SideWire uses `default-device` when configured, otherwise it auto-selects only when exactly one device is connected.

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

The shared wire protocol is version 9. Protocol changes require rebuilding both the desktop CLI and Android module; mismatched protocol versions are rejected during frame decoding.

## Security notes

Secure mode is the default and requires a successful pairing before privileged SideWire traffic is accepted. Pairing PINs are short-lived and are not retained as connection passwords. Insecure mode deliberately disables these protections and is intended only for trusted development networks. As with any root-capable remote-control service, keep SideWire's TCP ports behind a trusted LAN/VPN and avoid exposing them directly to the public Internet.
