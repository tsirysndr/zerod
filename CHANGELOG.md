# Changelog

All notable changes to `zerod` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Pre-1.0, breaking changes may land in any minor release.

## [Unreleased]

## [0.2.0] - 2026-06-11

### Added

- **mDNS / zeroconf discovery.** The server advertises itself as
  `_zerod._tcp.local.` via a new `zerod-discovery` crate (pure-Rust
  `mdns-sd`, no Avahi / Bonjour). The instance name defaults to the
  machine hostname and the published TXT records include the daemon
  version.
- **`zerod discover` subcommand** — lists every responder on the LAN
  with name, host, port, and advertised version.
- **`--name` / `ZEROD_NAME` flag** — when multiple servers respond,
  pick one by its mDNS instance name.
- **`--discover-timeout-ms` / `ZEROD_DISCOVER_TIMEOUT_MS` flag** —
  override the default 1500 ms browse window.
- **`[mdns]` section in `zerod.toml`** — `enabled` (default `true`) and
  `name` (empty → hostname).

### Changed

- **`--host` is now optional.** Omitting it (and `ZEROD_HOST`) triggers
  mDNS discovery and connects to the only responder. Previously it
  defaulted to `localhost`.
- IPv4 address selection during discovery skips loopback and Docker's
  `172.17.0.0/16` default bridge, so `zerod` running inside Docker or
  next to a `docker0` interface no longer leaks the bridge IP to clients.
- `zerod service install` post-install hint now reflects the
  discovery-first UX (`zerod system health` instead of
  `zerod --host <pi> system health`).

## [0.1.0]

Initial public release.

- gRPC server (`tonic`) with bearer-token auth and `tonic-reflection`.
- Subcommand clients in the same binary: `bluetooth`, `stream`,
  `systemd`, `config`, `volume`, `system`, `service`.
- HLS / MPEG-DASH player with `cpal` / `stdout` / `pipe` sinks.
- BlueZ wrapper, systemd D-Bus client, ALSA mixer, atomic config writes.
- `zerod service install` writes a `--user` systemd unit pinned with a
  random bearer token.
- Cross-compile setup for `arm-unknown-linux-gnueabihf`,
  `aarch64-unknown-linux-gnu`, and `x86_64-unknown-linux-gnu`.
- GitHub Actions release workflow + Homebrew tap.

[Unreleased]: https://github.com/tsirysndr/zerod/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/tsirysndr/zerod/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tsirysndr/zerod/releases/tag/v0.1.0
