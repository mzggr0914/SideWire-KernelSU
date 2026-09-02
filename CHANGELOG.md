# Changelog

## Unreleased

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
