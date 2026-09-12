# Changelog

## Unreleased

## 1.0.0

- add protocol 1.2 receive-window flow control for file chunks while preserving protocol 1.1 fallback behavior
- parallelize recursive file transfers with bounded `--jobs` concurrency and batch remote directory creation
- reduce transport hot-path contention by removing async locks from per-frame stream routing
- harden stream cancellation, writer shutdown, and secure-frame buffering under stalled or canceled transfers
- reduce Android outbound idle overhead with handshake-aware reconnect backoff, rate-limited failure logs, and startup log trimming
- keep the Android clipboard helper persistent across requests to avoid repeated `app_process`/JVM startup overhead
- standardize CI and release WebUI builds on pnpm 12.3.4

## 0.9.9

- use the configured SideWire device name as the Android PTY hostname so shell prompts show names like `A32` instead of `android`

## 0.9.8

- force the extensionless Android `sidewirectl` script to LF in Windows release checkouts and reject CRLF during packaging
- resolve the WebUI controller from active or staged KernelSU module paths so controls work immediately after install/update
- isolate logical-stream backpressure so a stalled PTY, logcat, or transfer cannot block the shared transport reader
- add protocol 1.1 stream cancellation with protocol 1.0 fallback behavior
- tolerate transient heartbeat delays and replace stale sessions when an outbound device reconnects
- make file push accept remote directories like ADB and surface destination errors before streaming the file

## 0.9.7

- keep Windows logcat/PTY/exec streaming alive across invalid or split UTF-8 bytes
- preserve raw stream bytes when stdout/stderr are redirected instead of attached to a Windows console

## 0.9.6

- detached WebUI-started Android daemon from the manager app cgroup via KernelSU module actions
- kept boot autostart on the direct KernelSU service path

## 0.9.5

- added negotiated connection heartbeats and faster outbound reconnects
- fixed Android shell identity execution under KernelSU
- fixed clipboard get/set/clear on Samsung Android 13 and provisioned the helper for shell access
