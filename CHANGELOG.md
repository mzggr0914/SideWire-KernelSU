# Changelog

## Unreleased

## 0.9.8

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
- made Windows local control Named Pipe startup more resilient to transient disconnects
- relicensed SideWire under GNU GPL v3 (`GPL-3.0-only`)
- added native macOS host builds for Apple Silicon and Intel

## 0.9.4

- made pairing independent of the desktop server lifecycle
- improved outbound reconnect behavior after pairing
- fixed pairing PIN generation on Android shell arithmetic
- added WebUI log auto-scroll and pairing copy cleanup

## 0.9.3

- fixed stale daemon recovery across module upgrades
- made pairing-listener bind failures fatal instead of leaving a half-working daemon

## 0.9.2

- removed ANSI control sequences from module logs

## 0.9.1

- improved Windows Named Pipe startup errors
- treated a missing daemon log as a normal empty state

## 0.9.0

- added clipboard support
- replaced local TCP control with Windows Named Pipe / Unix socket IPC
- added protocol 1.0 capability negotiation
- hardened pairing and moved Android trust state to persistent storage
