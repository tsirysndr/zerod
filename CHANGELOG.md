# Changelog

All notable changes to `zerod` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Pre-1.0, breaking changes may land in any minor release.

## [Unreleased]

## [0.3.1] - 2026-06-12

### Fixed

- **HLS playback on macOS no longer clicks/jumps every few seconds.**
  Four cumulative fixes, in order of audible impact:
  - **Continuous decode across HLS segments.** `decode_segment` is now
    a `StreamDecoder` struct that owns one `Box<dyn Decoder>` for the
    whole stream. The format reader is still rebuilt per segment, but
    the AAC decoder's state (predictor history, SBR/PS continuity,
    encoder priming) persists — so the ~50 ms of priming silence /
    decoder warm-up no longer reappears at every segment boundary.
    This is the model ffplay uses against libavcodec.
  - **`CpalSink` rewrite.** Replaced `Mutex<VecDeque<u8>>` + `Condvar`
    with an `rtrb` lock-free SPSC ring and moved the consumer half
    into the cpal callback. The CoreAudio realtime thread now does
    zero locks, allocations, or syscalls per buffer. Resampler upgraded
    from nearest-neighbour (which audibly aliased 44.1 → 48 kHz) to
    linear interpolation between adjacent source frames.
  - **1.5 s sink pre-buffer.** Resampler waits for ~1.5 s of source
    frames before priming so brief producer hiccups can't immediately
    underrun. Ring capacity bumped to 512 K samples so the bigger
    pre-buffer fits well under the 50 % cap.
  - **Background per-segment prefetch.** The playback loop no longer
    `.await`s `fetcher::prefetch`; it's `tokio::spawn`-ed instead, so
    HTTP latency for upcoming segments is hidden from the gap between
    writes. Cache miss in the main loop still falls back to a
    synchronous fetch, so correctness is preserved.

## [0.3.0] - 2026-06-11

### Added

- **Server-streaming `EventsService.Subscribe`** — a new `zerod-events`
  leaf crate owns an in-process `broadcast` channel that every
  subsystem publishes into. Subscribers filter by stable kind label
  (exact match like `stream.state` or `bt.*` wildcard). Slow
  subscribers see a synthetic `LaggedEvent` rather than a closed
  stream. The reserved variants (`SnapcastClientChanged`,
  `LibrespotStateChanged`, A2DP connect/disconnect) ship now so later
  features don't need a wire bump.
- **`zerod events tail [--filter …]` subcommand** — streams events as
  one JSON line per event.
- **`SnapcastService` + `zerod-snapcast` crate** — hand-rolled JSON-RPC
  2.0 client over TCP port 1705 with a single long-lived connection,
  exponential-backoff reconnect, per-request `oneshot` correlation,
  and fail-fast on calls made while disconnected. MVP verbs:
  `GetServerStatus`, `ListClients`, `ListSnapStreams`,
  `SetClientVolume/Latency/Name`, `SetGroupStream/Mute/Clients`. Push
  notifications (`Client.OnVolumeChanged`, …) are forwarded onto the
  event bus when `forward_notifications = true`.
- **`zerod snapcast {status, clients, streams, volume, latency, name,
  group-stream, group-mute, group-clients}` subcommands.**
- **`[snapcast]` config section** (`enabled` / `host` / `port` /
  `forward_notifications`).
- **Spotify Connect source** via a `librespot` subprocess
  (`crates/stream/src/sources/librespot.rs`). Spawns `librespot
  --backend pipe --format S16` and pipes its 44.1 kHz / 2ch S16LE
  stdout through the existing `AudioSink` so per-stream gain Just
  Works. `kill_on_drop` on the `Child` makes `Player::cancel()` reap
  it cleanly.
- **`StreamService.SpotifyStart` / `SpotifyStop` RPCs** plus a new
  `PlaybackSource` enum (`Hls` / `Dash` / `Spotify`) on
  `StatusResponse` so clients can tell what's playing.
- **`zerod stream spotify {start, stop}` subcommands.**
- **`[librespot]` config section** (`enabled` / `binary` / `name` /
  `bitrate` / `cache_path`). Disabled by default — `SpotifyStart`
  returns `FAILED_PRECONDITION` until you flip it on.
- **A2DP sink mode (Pi-as-Bluetooth-speaker).** A new
  `crates/bluetooth/src/agent.rs` registers a BlueZ pairing agent at
  server boot. `RequestConfirmation` publishes a
  `BluetoothPairingRequest` event and either auto-accepts (kiosk
  mode) or parks on a per-address `oneshot` waiting for
  `BluetoothService.RespondPairing`; a 30 s timeout falls back to
  rejection so stuck prompts can't leak the channel.
  `AuthorizeService` accepts the A2DP Source UUID
  (`0000110a-…`) and emits `BluetoothA2dpConnected`.
- **`SetDiscoverable`, `RespondPairing`, `A2dpEnable`, `A2dpDisable`
  RPCs.** `A2dpEnable` preflights `bluealsa-aplay.service` via the
  existing systemd allowlist and returns a helpful "install
  bluez-alsa-utils" message if the unit is missing.
- **`zerod a2dp {enable, disable}` and `zerod bluetooth
  {discoverable, respond-pairing}` subcommands.**
- **`[bluetooth.a2dp]` config section** (`enabled` /
  `bluealsa_aplay_unit` / `auto_accept_pairings` / `adapter_alias` /
  `discoverable_on_boot` / `discoverable_timeout_secs`). The
  `bluealsa_aplay_unit` is auto-appended to the systemd allowlist
  when `enabled = true` so users don't have to remember to list it
  under `[systemd].units`.

### Changed

- **Stream subsystem now emits state-transition events** —
  `StreamStateChanged` from `Player::set_state` (covers Stopped /
  Buffering / Playing / Paused / Errored), `StreamVolumeChanged` on
  per-stream gain updates.
- **Bluetooth, systemd, and volume subsystems also emit events** on
  successful operations: `BluetoothDeviceChanged` after pair /
  connect / disconnect / remove, `SystemdUnitState` after every
  successful zbus verb (re-reads status once), `VolumeChanged` after
  ALSA mixer set / mute.
- `Player` gains a `source: AtomicU8` field exposed via
  `StatusResponse.source`. The HLS / DASH branch sets it from
  `ManifestKind`; the Spotify branch sets `Spotify`.
- `bluer` pinned to `=0.17.4` exact — the agent callback shape varies
  across minor versions and silent rewires of the pairing flow are
  worse than a build break.

### Notes

- Spotify Connect requires `librespot` installed on the device
  (`apt install librespot` on Debian / Ubuntu / Raspberry Pi OS).
  `librespot` itself is not bundled — `zerod` only supervises the
  subprocess.
- A2DP audio routing leans on `bluealsa-aplay` from
  `bluez-alsa-utils`. `zerod` registers the BlueZ pairing agent and
  flips the adapter; the actual SBC / AAC decode happens in
  `bluealsa-aplay`.
- A2DP pair flows that need a PIN code or passkey input fall through
  to BlueZ's default rejection — legacy headphones using pre-2.1
  pairing won't work yet.

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

[Unreleased]: https://github.com/tsirysndr/zerod/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/tsirysndr/zerod/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tsirysndr/zerod/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tsirysndr/zerod/releases/tag/v0.1.0
