# Security

SideWire can expose root-capable Android operations over the network, so security bugs should not be filed as public issues before a fix is available.

## Supported versions

The latest `0.9.x` release is supported while SideWire is pre-1.0. Older pre-release versions may not receive security fixes.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting when it is available for the repository.

Please include:

- SideWire version
- Android version / ROM and KernelSU version
- host OS
- connection mode (`inbound` or `outbound`)
- whether the session was `secure` or `insecure`
- reproduction steps and expected impact

Avoid including real pairing secrets, trust-store contents, or other credentials in reports.

## Network exposure

Secure mode is the default. Insecure mode disables authentication and encryption and should only be used on a network you fully trust.

SideWire is designed for trusted LAN/VPN use. Do not expose its listening ports directly to the public Internet.
