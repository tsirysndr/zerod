# zerod

Headless audio + Bluetooth + systemd control daemon for Linux audio appliances
(Raspberry Pi, single-board computers running Snapcast, shairport-sync,
squeezelite, …). One binary, one gRPC port, one TOML file.

`zerod` is both:

- a **server** — exposes a gRPC API over `tonic` to control BlueZ, play HLS /
  MPEG-DASH audio streams to a configurable sink (cpal / stdout / pipe), drive
  systemd units, and remotely read/write a fixed set of config files.
- a **client CLI** — talks to another `zerod` over the same gRPC API.

Same binary on both ends. Run `zerod` with no arguments and it boots the
server; run `zerod stream play …` and it acts as a client.

## Status

Early. Pre-1.0, breaking changes expected.

## Features

- **Bluetooth** — scan / list / pair / connect / disconnect / remove over
  BlueZ (Linux-only via `bluer`). Non-Linux builds compile but return
  `Unimplemented`.
- **HLS / MPEG-DASH player** — fetch + demux + decode (symphonia) → S16LE PCM.
  Three sinks selectable per `Play` call:
  - `cpal` — default audio device (or a named device)
  - `stdout` — raw interleaved S16LE little-endian PCM on stdout
  - `pipe` — same, into a named FIFO (auto-reopens on broken pipe)
- **Systemd control** — start / stop / restart / reload / enable / disable /
  status via the system D-Bus, restricted to an allowlist. Linux-only.
- **Volume control** — get / set volume and mute / unmute against any ALSA
  selem (Master, PCM, …) on any card via `alsa-lib`. Works on bare ALSA and
  on PipeWire/PulseAudio via ALSA-mixer emulation. Linux-only. Plus a
  per-stream software gain applied in the player loop for the HLS/DASH
  session, independent of the system mixer.
- **Remote config edit** — atomic read/write of a fixed set of files
  (`snapserver.conf`, `shairport-sync.conf`, …), with an optional
  reload-or-restart of the bound unit on every write.
- **Auth** — bearer token. Three sources, in order: `zerod.toml`,
  `ZEROD_BEARER_TOKEN` env var, or a 32-byte random one generated and logged
  at startup.
- **Reflection** — `tonic-reflection` is wired up, so `grpcurl
  -plaintext localhost:50151 list` works out of the box.

## Build

Native build (whatever host you're on):

```
cargo build --release
```

System dependencies on Linux: `protoc` (≥ 3.15), `libasound2-dev`,
`libdbus-1-dev`, `pkg-config`.

### Cross-compiling

`Cross.toml` + a per-target `Dockerfile.<triple>` are checked in. Each
Dockerfile pins protoc 25.1 (the cross base images ship a protoc too old
for proto3 `optional`) and installs the multiarch ALSA / D-Bus dev libs.

| Use case                                 | Triple                        | Command                                                      |
| ---------------------------------------- | ----------------------------- | ------------------------------------------------------------ |
| Raspberry Pi 1 / Zero / 2 / Pi OS 32-bit | `arm-unknown-linux-gnueabihf` | `cross build --release --target arm-unknown-linux-gnueabihf` |
| Raspberry Pi 3 / 4 / 5 / 64-bit ARM SBCs | `aarch64-unknown-linux-gnu`   | `cross build --release --target aarch64-unknown-linux-gnu`   |
| Generic Linux x86_64 (NUC, server, …)    | `x86_64-unknown-linux-gnu`    | `cross build --release --target x86_64-unknown-linux-gnu`    |

After a successful build the binary lands at
`target/<triple>/release/zerod` — scp it to the device, drop a
`zerod.toml` next to it (or at `/etc/zerod.toml`), and run.

If you edit a Dockerfile, force `cross` to rebuild the image with
`CROSS_REBUILD=1 cross build …` (cross caches the image per-target).

## Configuration

Settings live in `zerod.toml`. Search order:

1. path passed via `--config`
2. `./zerod.toml`
3. `$XDG_CONFIG_HOME/zerod/zerod.toml` (or `~/.config/zerod/zerod.toml`)
4. `/etc/zerod.toml`

See `zerod.toml.example` for the full schema. Minimal example:

```toml
[server]
bind = "127.0.0.1:50151"
bearer_token = ""             # empty → ZEROD_BEARER_TOKEN env → random

[systemd]
units = ["snapserver.service", "shairport-sync.service"]

[[configs]]
key = "snapserver"
path = "/etc/snapserver.conf"
unit = "snapserver.service"
```

If no `zerod.toml` is found, `zerod` runs with defaults (loopback, no
systemd allowlist, no managed configs) and emits a warning.

## Usage

### Server

```
zerod                                       # default; reads zerod.toml
zerod serve --config /etc/zerod.toml        # explicit
ZEROD_BEARER_TOKEN=hunter2 zerod            # fixed token via env
```

Bind / token / allowlist come from `zerod.toml`. A randomly-generated token
is printed once at startup if nothing is configured.

### Client

Client subcommands default to `--host localhost --port 50151`. Each global
flag also reads from an env var so you can pin the target once per shell
session and skip the flags entirely:

| Flag             | Env var              |
| ---------------- | -------------------- |
| `--host`         | `ZEROD_HOST`         |
| `--port`         | `ZEROD_PORT`         |
| `--bearer-token` | `ZEROD_BEARER_TOKEN` |

```
export ZEROD_HOST=pi.lan
export ZEROD_BEARER_TOKEN="$(cat ~/.zerod-token)"
zerod systemd status snapserver.service     # uses pi.lan + token from env
```

```
zerod bluetooth scan --timeout-secs 5
zerod bluetooth connect AA:BB:CC:DD:EE:FF

zerod stream play https://example.com/audio.m3u8 --output cpal
zerod stream play https://example.com/audio.mpd --output pipe --pipe-path /tmp/audio.pcm
zerod stream status
zerod stream stop

zerod systemd list
zerod systemd restart snapserver.service

zerod volume list                          # all ALSA selems on default card
zerod volume get                           # Master on default card
zerod volume set 70                        # Master → 70%
zerod volume set 50 --control PCM          # specific control
zerod volume mute
zerod volume unmute

zerod stream volume set 80                 # per-stream gain (HLS/DASH session)
zerod stream volume get

zerod config list
zerod config get snapserver
zerod config put snapserver ./snapserver.conf --action restart

zerod --host pi.lan --bearer-token "$(cat ~/.zerod-token)" systemd status snapserver.service
```

### gRPC directly

```
grpcurl -plaintext localhost:50151 list
grpcurl -plaintext -H 'authorization: Bearer …' \
  -d '{"timeout_secs": 5}' localhost:50151 zerod.v1alpha1.BluetoothService/Scan
```

## Security

There is intentionally no TLS in v1. The default bind is `127.0.0.1`. If you
need to drive `zerod` across a network, put it behind WireGuard / Tailscale /
SSH; do not expose `:50151` directly. The bearer token is the only line of
defence against requests that reach the listening socket.

Systemd actions and config writes are gated by the allowlist in `zerod.toml`
— `zerod` cannot be turned into a generic remote `systemctl`.

## Layout

```
zerod/
├── Cargo.toml                          # workspace root, also the binary
├── src/main.rs                         # clap CLI (server + client modes)
├── zerod.toml.example
├── Cross.toml
├── Dockerfile.arm-unknown-linux-gnueabihf
└── crates/
    ├── proto/      # tonic_build over .proto files (zerod.v1alpha1)
    ├── bluetooth/  # bluer wrapper (target_os="linux" gated)
    ├── stream/     # HLS/DASH player + AudioSink trait + 3 sinks
    ├── systemd/    # zbus systemd1 client (target_os="linux" gated)
    ├── config/     # ManagedConfig registry, atomic write
    └── server/     # tonic services + settings loader + bearer interceptor
```

## License

MIT. See [LICENSE](LICENSE).
