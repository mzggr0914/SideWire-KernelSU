# SideWire

ADB-independent native control bridge for rooted Android devices.

Current prototype goals:
- Rust native Android daemon (`sidewired`)
- Rust desktop CLI (`sidewire`)
- Shared framed protocol with stream IDs
- Inbound and outbound TCP modes
- Remote command execution with stdout/stderr/exit status
- KernelSU module skeleton

## Interactive shell

```powershell
sidewire shell
sidewire shell --as root
```

The default mode uses reliable host-side line input. Use `sidewire shell --raw`
when Android mksh should receive every key immediately, including Tab for its
built-in file and directory completion.

## Desktop smoke test

```powershell
cargo run -p sidewired -- --mode inbound --listen 127.0.0.1:58321
cargo run -p sidewire -- exec --addr 127.0.0.1:58321 cmd /C echo SideWire
```

## Outbound smoke test

```powershell
cargo run -p sidewire -- listen --bind 127.0.0.1:58321 --program whoami
cargo run -p sidewired -- --mode outbound --server 127.0.0.1:58321
```
