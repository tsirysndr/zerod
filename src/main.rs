//! zerod — one binary, two modes.
//!
//! `zerod` (no args)         → run as a gRPC server (reads zerod.toml).
//! `zerod serve …`           → explicit server invocation.
//! `zerod bluetooth scan`    → client subcommands. With no --host, the
//! `zerod stream play …`       client browses mDNS (`_zerod._tcp.local.`)
//! `zerod systemd start …`     and connects to the only responder. Use
//! `zerod config get …`        --name to disambiguate when several reply,
//! `zerod discover`            or --host to bypass discovery entirely.

use anyhow::{anyhow, Context, Result};
use clap::builder::styling::{Color, RgbColor, Style, Styles};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::Request;
use zerod_proto::v1alpha1 as pb;

mod service;

// Charm-inspired neon violet palette for the clap help menu.
// #7D56F4 — Charm signature purple-violet-blue (headers, usage line)
// #A78BFA — bright violet (commands, flags, valid values)
// #C4B5FD — soft lavender (placeholders / value names)
// #FF5C8A — pink-red (errors, invalid values)
const CHARM_VIOLET: Color = Color::Rgb(RgbColor(0x7D, 0x56, 0xF4));
const NEON_VIOLET: Color = Color::Rgb(RgbColor(0xA7, 0x8B, 0xFA));
const SOFT_LAVENDER: Color = Color::Rgb(RgbColor(0xC4, 0xB5, 0xFD));
const NEON_PINK_ERR: Color = Color::Rgb(RgbColor(0xFF, 0x5C, 0x8A));

const STYLES: Styles = Styles::styled()
    .header(Style::new().bold().underline().fg_color(Some(CHARM_VIOLET)))
    .usage(Style::new().bold().fg_color(Some(CHARM_VIOLET)))
    .literal(Style::new().bold().fg_color(Some(NEON_VIOLET)))
    .placeholder(Style::new().fg_color(Some(SOFT_LAVENDER)))
    .valid(Style::new().bold().fg_color(Some(NEON_VIOLET)))
    .invalid(Style::new().bold().fg_color(Some(NEON_PINK_ERR)))
    .error(Style::new().bold().fg_color(Some(NEON_PINK_ERR)));

#[derive(Parser)]
#[command(
    name = "zerod",
    version,
    about = "headless audio/bluetooth/systemd control daemon",
    styles = STYLES,
)]
struct Cli {
    /// Path to zerod.toml. Server mode only.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Client mode: target server host. When omitted, discovers a server via
    /// mDNS on the LAN. Ignored for `serve`.
    #[arg(long, global = true, env = "ZEROD_HOST")]
    host: Option<String>,

    /// Client mode: target server port. Ignored for `serve` and when --host
    /// is omitted (the discovered port is used).
    #[arg(long, global = true, env = "ZEROD_PORT", default_value_t = 50151)]
    port: u16,

    /// Client mode: bearer token to send.
    #[arg(long, global = true, env = "ZEROD_BEARER_TOKEN")]
    bearer_token: Option<String>,

    /// Client mode: when discovering via mDNS, pick the responder whose
    /// instance name matches this exactly. Has no effect when --host is set.
    #[arg(long, global = true, env = "ZEROD_NAME")]
    name: Option<String>,

    /// Client mode: how long to browse mDNS before giving up (milliseconds).
    #[arg(long, global = true, env = "ZEROD_DISCOVER_TIMEOUT_MS", default_value_t = 1500)]
    discover_timeout_ms: u64,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run as a gRPC server (default when no subcommand is given).
    Serve,
    /// Bluetooth control (client).
    #[command(subcommand)]
    Bluetooth(BluetoothCmd),
    /// HLS/DASH stream control (client).
    #[command(subcommand)]
    Stream(StreamCmd),
    /// Systemd unit control (client).
    #[command(subcommand)]
    Systemd(SystemdCmd),
    /// Managed config files (client).
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Server version / health (client).
    #[command(subcommand)]
    System(SystemCmd),
    /// System volume control via ALSA mixer (client).
    #[command(subcommand)]
    Volume(VolumeCmd),
    /// Manage zerod's own systemd --user unit (Linux only).
    #[command(subcommand)]
    Service(ServiceCmd),
    /// Subscribe to server-side events (server-streaming).
    #[command(subcommand)]
    Events(EventsCmd),
    /// Browse mDNS for zerod servers on the LAN.
    Discover,
}

#[derive(Subcommand)]
enum EventsCmd {
    /// Print events as JSON, one per line, until interrupted.
    Tail {
        /// Kind filter(s). Empty → every event. Exact match (`stream.state`)
        /// or `.*` suffix wildcard (`bt.*`). Repeat the flag for OR.
        #[arg(long)]
        filter: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Write ~/.config/systemd/user/zerod.service pointing at this binary.
    /// A bearer token is pinned into the unit so it doesn't change on every
    /// restart — pass --token to choose one, otherwise a random 32-byte hex
    /// token is generated. Re-running with --force rotates the token unless
    /// --token is supplied.
    Install {
        /// Overwrite an existing unit file (rotates the bearer token unless --token is given).
        #[arg(long)]
        force: bool,
        /// Pin this exact bearer token into the unit instead of generating a random one.
        #[arg(long)]
        token: Option<String>,
    },
    /// Remove ~/.config/systemd/user/zerod.service.
    Uninstall,
    /// Print where the unit file would be installed.
    Path,
}

#[derive(Subcommand)]
enum BluetoothCmd {
    Scan {
        #[arg(long, default_value_t = 10)]
        timeout_secs: u32,
    },
    List,
    Pair { address: String },
    Connect { address: String },
    Disconnect { address: String },
    Remove { address: String },
}

#[derive(Subcommand)]
enum StreamCmd {
    Play {
        url: String,
        #[arg(long, value_enum, default_value_t = OutputArg::Cpal)]
        output: OutputArg,
        #[arg(long)]
        pipe_path: Option<String>,
    },
    Pause,
    Resume,
    Stop,
    Status,
    /// Per-stream software gain (0..=100), independent of the system mixer.
    #[command(subcommand)]
    Volume(StreamVolumeCmd),
}

#[derive(Subcommand)]
enum StreamVolumeCmd {
    Get,
    Set { percent: u32 },
}

#[derive(Subcommand)]
enum VolumeCmd {
    /// List ALSA selems on a card (default card if --card omitted).
    List {
        #[arg(long)]
        card: Option<String>,
    },
    Get {
        #[arg(long)]
        card: Option<String>,
        #[arg(long)]
        control: Option<String>,
        #[arg(long, default_value_t = 0)]
        index: u32,
    },
    Set {
        percent: u32,
        #[arg(long)]
        card: Option<String>,
        #[arg(long)]
        control: Option<String>,
        #[arg(long, default_value_t = 0)]
        index: u32,
    },
    Mute {
        #[arg(long)]
        card: Option<String>,
        #[arg(long)]
        control: Option<String>,
        #[arg(long, default_value_t = 0)]
        index: u32,
    },
    Unmute {
        #[arg(long)]
        card: Option<String>,
        #[arg(long)]
        control: Option<String>,
        #[arg(long, default_value_t = 0)]
        index: u32,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum OutputArg {
    Cpal,
    Stdout,
    Pipe,
}

#[derive(Subcommand)]
enum SystemdCmd {
    List,
    Status { name: String },
    Start { name: String },
    Stop { name: String },
    Restart { name: String },
    Reload { name: String },
    Enable { name: String },
    Disable { name: String },
}

#[derive(Subcommand)]
enum ConfigCmd {
    List,
    Get {
        key: String,
    },
    /// Write the contents of a local file as the new config and optionally
    /// reload/restart the bound unit.
    Put {
        key: String,
        file: PathBuf,
        #[arg(long, value_enum, default_value_t = ActionArg::None)]
        action: ActionArg,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ActionArg {
    None,
    Reload,
    Restart,
}

#[derive(Subcommand)]
enum SystemCmd {
    Version,
    Health,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let Cli {
        config,
        host,
        port,
        bearer_token,
        name,
        discover_timeout_ms,
        command,
    } = cli;
    let discover_timeout = Duration::from_millis(discover_timeout_ms);
    let needs_endpoint = !matches!(
        command,
        None | Some(Command::Serve) | Some(Command::Service(_)) | Some(Command::Discover)
    );
    let endpoint = if needs_endpoint {
        Some(resolve_endpoint(host.as_deref(), port, name.as_deref(), discover_timeout)?)
    } else {
        None
    };
    match command {
        None | Some(Command::Serve) => run_server(config.as_deref()).await,
        Some(Command::Service(cmd)) => run_service(cmd),
        Some(Command::Discover) => run_discover(discover_timeout),
        Some(Command::Bluetooth(cmd)) => run_bluetooth(&endpoint.unwrap(), bearer_token, cmd).await,
        Some(Command::Stream(cmd)) => run_stream(&endpoint.unwrap(), bearer_token, cmd).await,
        Some(Command::Systemd(cmd)) => run_systemd(&endpoint.unwrap(), bearer_token, cmd).await,
        Some(Command::Config(cmd)) => run_config(&endpoint.unwrap(), bearer_token, cmd).await,
        Some(Command::System(cmd)) => run_system(&endpoint.unwrap(), bearer_token, cmd).await,
        Some(Command::Volume(cmd)) => run_volume(&endpoint.unwrap(), bearer_token, cmd).await,
        Some(Command::Events(cmd)) => run_events(&endpoint.unwrap(), bearer_token, cmd).await,
    }
}

struct Endpoint {
    host: String,
    port: u16,
}

fn resolve_endpoint(
    host: Option<&str>,
    port: u16,
    name: Option<&str>,
    timeout: Duration,
) -> Result<Endpoint> {
    if let Some(h) = host.filter(|s| !s.is_empty()) {
        return Ok(Endpoint { host: h.to_string(), port });
    }
    let mut found = zerod_discovery::discover(timeout).context("mDNS browse")?;
    if let Some(n) = name.filter(|s| !s.is_empty()) {
        found.retain(|d| d.name == n);
    }
    match found.len() {
        0 => Err(anyhow!(
            "no zerod server found on the LAN within {}ms. \
             Pass --host explicitly, or check that the daemon is running with [mdns] enabled.",
            timeout.as_millis(),
        )),
        1 => {
            let d = found.remove(0);
            let h = d.best_host().ok_or_else(|| {
                anyhow!("discovered server {:?} has no usable IPv4 address", d.name)
            })?;
            tracing::info!("mDNS: using {} ({}:{})", d.name, h, d.port);
            Ok(Endpoint { host: h, port: d.port })
        }
        _ => {
            let mut msg = String::from(
                "multiple zerod servers responded. Re-run with --name <name> to pick one:\n",
            );
            for d in &found {
                let h = d.best_host().unwrap_or_else(|| "?".to_string());
                msg.push_str(&format!("  {:30}  {}:{}\n", d.name, h, d.port));
            }
            Err(anyhow!(msg))
        }
    }
}

fn run_discover(timeout: Duration) -> Result<()> {
    let found = zerod_discovery::discover(timeout).context("mDNS browse")?;
    if found.is_empty() {
        println!("no zerod servers found on the LAN within {}ms", timeout.as_millis());
        return Ok(());
    }
    for d in &found {
        let host = d.best_host().unwrap_or_else(|| "?".to_string());
        let version = d
            .properties
            .get("version")
            .map(String::as_str)
            .unwrap_or("?");
        println!("{:30}  {}:{}  version={}", d.name, host, d.port, version);
    }
    Ok(())
}

fn run_service(cmd: ServiceCmd) -> Result<()> {
    match cmd {
        ServiceCmd::Install { force, token } => {
            let installed = service::install(force, token)?;
            println!("Wrote {}", installed.path.display());
            println!();
            println!("Bearer token (pinned in the unit, mode 0600):");
            println!("  {}", installed.token);
            println!();
            println!("Next steps:");
            println!("  systemctl --user daemon-reload");
            println!("  systemctl --user enable --now zerod.service");
            println!();
            println!("Then check it's healthy:");
            println!("  systemctl --user status zerod.service");
            println!("  journalctl --user -u zerod.service -f");
            println!();
            println!("So the service survives logout / starts on boot:");
            println!("  sudo loginctl enable-linger \"$USER\"");
            println!();
            println!("Use the token from any client (auto-discovers via mDNS):");
            println!("  export ZEROD_BEARER_TOKEN={}", installed.token);
            println!("  zerod system health");
            println!();
            println!("Or list every responder on the LAN:");
            println!("  zerod discover");
            Ok(())
        }
        ServiceCmd::Uninstall => match service::uninstall()? {
            Some(path) => {
                println!("Removed {}", path.display());
                println!();
                println!("Tell systemd to forget it:");
                println!("  systemctl --user disable --now zerod.service");
                println!("  systemctl --user daemon-reload");
                Ok(())
            }
            None => {
                println!("No unit file installed at {}", service::unit_path()?.display());
                Ok(())
            }
        },
        ServiceCmd::Path => {
            println!("{}", service::unit_path()?.display());
            Ok(())
        }
    }
}

async fn run_server(config: Option<&std::path::Path>) -> Result<()> {
    let settings = zerod_server::load_settings(config)?;
    zerod_server::serve(settings).await
}

// --- client helpers ---------------------------------------------------------

async fn channel(ep: &Endpoint) -> Result<Channel> {
    let url = format!("http://{}:{}", ep.host, ep.port);
    Channel::from_shared(url.clone())
        .with_context(|| format!("invalid endpoint {url}"))?
        .connect()
        .await
        .with_context(|| format!("connect {url}"))
}

fn attach_token<T>(mut req: Request<T>, token: &Option<String>) -> Request<T> {
    if let Some(t) = token.as_deref().filter(|s| !s.is_empty()) {
        let v: MetadataValue<_> = format!("Bearer {t}").parse().expect("bearer token must be ascii");
        req.metadata_mut().insert("authorization", v);
    }
    req
}

fn print_json(v: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

// --- bluetooth -------------------------------------------------------------

async fn run_bluetooth(ep: &Endpoint, token: Option<String>, cmd: BluetoothCmd) -> Result<()> {
    let ch = channel(ep).await?;
    let mut client = pb::bluetooth_service_client::BluetoothServiceClient::new(ch);
    match cmd {
        BluetoothCmd::Scan { timeout_secs } => {
            let r = client
                .scan(attach_token(Request::new(pb::ScanRequest { timeout_secs }), &token))
                .await?
                .into_inner();
            for d in r.devices {
                println!(
                    "{}  {:30}  paired={} trusted={} connected={} rssi={:?}",
                    d.address, d.name, d.paired, d.trusted, d.connected, d.rssi
                );
            }
        }
        BluetoothCmd::List => {
            let r = client
                .list_devices(attach_token(Request::new(pb::ListDevicesRequest {}), &token))
                .await?
                .into_inner();
            for d in r.devices {
                println!(
                    "{}  {:30}  paired={} trusted={} connected={} rssi={:?}",
                    d.address, d.name, d.paired, d.trusted, d.connected, d.rssi
                );
            }
        }
        BluetoothCmd::Pair { address } => {
            client.pair(attach_token(Request::new(pb::PairRequest { address }), &token)).await?;
            println!("ok");
        }
        BluetoothCmd::Connect { address } => {
            client.connect_device(attach_token(Request::new(pb::ConnectDeviceRequest { address }), &token)).await?;
            println!("ok");
        }
        BluetoothCmd::Disconnect { address } => {
            client.disconnect(attach_token(Request::new(pb::DisconnectRequest { address }), &token)).await?;
            println!("ok");
        }
        BluetoothCmd::Remove { address } => {
            client.remove(attach_token(Request::new(pb::RemoveRequest { address }), &token)).await?;
            println!("ok");
        }
    }
    Ok(())
}

// --- stream ----------------------------------------------------------------

async fn run_stream(ep: &Endpoint, token: Option<String>, cmd: StreamCmd) -> Result<()> {
    let ch = channel(ep).await?;
    let mut client = pb::stream_service_client::StreamServiceClient::new(ch);
    match cmd {
        StreamCmd::Play { url, output, pipe_path } => {
            let output = match output {
                OutputArg::Cpal => pb::AudioOutput::Cpal,
                OutputArg::Stdout => pb::AudioOutput::Stdout,
                OutputArg::Pipe => pb::AudioOutput::Pipe,
            } as i32;
            client
                .play(attach_token(
                    Request::new(pb::PlayRequest {
                        url,
                        output,
                        pipe_path,
                        cpal_device: None,
                    }),
                    &token,
                ))
                .await?;
            println!("ok");
        }
        StreamCmd::Pause => {
            client.pause(attach_token(Request::new(pb::PauseRequest {}), &token)).await?;
            println!("ok");
        }
        StreamCmd::Resume => {
            client.resume(attach_token(Request::new(pb::ResumeRequest {}), &token)).await?;
            println!("ok");
        }
        StreamCmd::Stop => {
            client.stop(attach_token(Request::new(pb::StopRequest {}), &token)).await?;
            println!("ok");
        }
        StreamCmd::Status => {
            let r = client
                .status(attach_token(Request::new(pb::StatusRequest {}), &token))
                .await?
                .into_inner();
            let state = pb::PlayerState::try_from(r.state).unwrap_or(pb::PlayerState::Unspecified);
            let out = pb::AudioOutput::try_from(r.output).unwrap_or(pb::AudioOutput::Unspecified);
            println!(
                "state={state:?} url={} position_ms={} duration_ms={} is_live={} output={out:?} volume={}% error={:?}",
                r.url, r.position_ms, r.duration_ms, r.is_live, r.volume_percent, r.error,
            );
        }
        StreamCmd::Volume(cmd) => match cmd {
            StreamVolumeCmd::Get => {
                let r = client
                    .get_stream_volume(attach_token(Request::new(pb::GetStreamVolumeRequest {}), &token))
                    .await?
                    .into_inner();
                println!("{}%", r.volume_percent);
            }
            StreamVolumeCmd::Set { percent } => {
                client
                    .set_stream_volume(attach_token(
                        Request::new(pb::SetStreamVolumeRequest { volume_percent: percent }),
                        &token,
                    ))
                    .await?;
                println!("ok");
            }
        },
    }
    Ok(())
}

async fn run_volume(ep: &Endpoint, token: Option<String>, cmd: VolumeCmd) -> Result<()> {
    let ch = channel(ep).await?;
    let mut client = pb::volume_service_client::VolumeServiceClient::new(ch);
    let mk = |card: Option<String>, control: Option<String>, index: u32| pb::MixerSelector {
        card: card.unwrap_or_default(),
        control: control.unwrap_or_default(),
        index,
    };
    match cmd {
        VolumeCmd::List { card } => {
            let r = client
                .list_mixers(attach_token(Request::new(pb::ListMixersRequest { card }), &token))
                .await?
                .into_inner();
            for m in r.mixers {
                println!(
                    "{:20}  index={}  volume={}  switch={}",
                    m.control, m.index, m.has_volume, m.has_switch
                );
            }
        }
        VolumeCmd::Get { card, control, index } => {
            let r = client
                .get_volume(attach_token(
                    Request::new(pb::GetVolumeRequest { mixer: Some(mk(card, control, index)) }),
                    &token,
                ))
                .await?
                .into_inner();
            if let Some(s) = r.status {
                println!("{}%  muted={}", s.volume_percent, s.muted);
            }
        }
        VolumeCmd::Set { percent, card, control, index } => {
            client
                .set_volume(attach_token(
                    Request::new(pb::SetVolumeRequest {
                        mixer: Some(mk(card, control, index)),
                        volume_percent: percent,
                    }),
                    &token,
                ))
                .await?;
            println!("ok");
        }
        VolumeCmd::Mute { card, control, index } => {
            client
                .set_mute(attach_token(
                    Request::new(pb::SetMuteRequest {
                        mixer: Some(mk(card, control, index)),
                        muted: true,
                    }),
                    &token,
                ))
                .await?;
            println!("ok");
        }
        VolumeCmd::Unmute { card, control, index } => {
            client
                .set_mute(attach_token(
                    Request::new(pb::SetMuteRequest {
                        mixer: Some(mk(card, control, index)),
                        muted: false,
                    }),
                    &token,
                ))
                .await?;
            println!("ok");
        }
    }
    Ok(())
}

// --- systemd ---------------------------------------------------------------

async fn run_systemd(ep: &Endpoint, token: Option<String>, cmd: SystemdCmd) -> Result<()> {
    let ch = channel(ep).await?;
    let mut client = pb::systemd_service_client::SystemdServiceClient::new(ch);
    match cmd {
        SystemdCmd::List => {
            let r = client
                .list_managed_units(attach_token(Request::new(pb::ListManagedUnitsRequest {}), &token))
                .await?
                .into_inner();
            for u in r.units {
                let state = pb::UnitActiveState::try_from(u.active_state).unwrap_or(pb::UnitActiveState::Unspecified);
                println!(
                    "{:35}  {:?}  sub={}  enabled={}  {}",
                    u.name, state, u.sub_state, u.enabled, u.description
                );
            }
        }
        SystemdCmd::Status { name } => {
            let r = client
                .status(attach_token(Request::new(pb::StatusRequestUnit { name }), &token))
                .await?
                .into_inner();
            if let Some(u) = r.unit {
                let state = pb::UnitActiveState::try_from(u.active_state).unwrap_or(pb::UnitActiveState::Unspecified);
                println!(
                    "{:35}  {:?}  sub={}  enabled={}  {}",
                    u.name, state, u.sub_state, u.enabled, u.description
                );
            }
        }
        SystemdCmd::Start { name } => { client.start(attach_token(Request::new(pb::UnitRequest { name }), &token)).await?; println!("ok"); }
        SystemdCmd::Stop { name } => { client.stop(attach_token(Request::new(pb::UnitRequest { name }), &token)).await?; println!("ok"); }
        SystemdCmd::Restart { name } => { client.restart(attach_token(Request::new(pb::UnitRequest { name }), &token)).await?; println!("ok"); }
        SystemdCmd::Reload { name } => { client.reload(attach_token(Request::new(pb::UnitRequest { name }), &token)).await?; println!("ok"); }
        SystemdCmd::Enable { name } => { client.enable(attach_token(Request::new(pb::UnitRequest { name }), &token)).await?; println!("ok"); }
        SystemdCmd::Disable { name } => { client.disable(attach_token(Request::new(pb::UnitRequest { name }), &token)).await?; println!("ok"); }
    }
    Ok(())
}

// --- config ----------------------------------------------------------------

async fn run_config(ep: &Endpoint, token: Option<String>, cmd: ConfigCmd) -> Result<()> {
    let ch = channel(ep).await?;
    let mut client = pb::config_service_client::ConfigServiceClient::new(ch);
    match cmd {
        ConfigCmd::List => {
            let r = client
                .list_configs(attach_token(Request::new(pb::ListConfigsRequest {}), &token))
                .await?
                .into_inner();
            for c in r.configs {
                println!("{:20}  {}  unit={}", c.key, c.path, c.unit);
            }
        }
        ConfigCmd::Get { key } => {
            let r = client
                .get_config(attach_token(Request::new(pb::GetConfigRequest { key }), &token))
                .await?
                .into_inner();
            if let Some(c) = r.config {
                eprintln!("# {} ← {} (unit={})", c.key, c.path, c.unit);
            }
            print!("{}", r.content);
        }
        ConfigCmd::Put { key, file, action } => {
            let content = tokio::fs::read_to_string(&file)
                .await
                .with_context(|| format!("read {}", file.display()))?;
            let action = match action {
                ActionArg::None => pb::PostWriteAction::None,
                ActionArg::Reload => pb::PostWriteAction::Reload,
                ActionArg::Restart => pb::PostWriteAction::Restart,
            } as i32;
            let r = client
                .put_config(attach_token(
                    Request::new(pb::PutConfigRequest { key, content, action }),
                    &token,
                ))
                .await?
                .into_inner();
            print_json(&serde_json::json!({"action_applied": r.action_applied}))?;
        }
    }
    Ok(())
}

// --- events ----------------------------------------------------------------

async fn run_events(ep: &Endpoint, token: Option<String>, cmd: EventsCmd) -> Result<()> {
    let ch = channel(ep).await?;
    let mut client = pb::events_service_client::EventsServiceClient::new(ch);
    match cmd {
        EventsCmd::Tail { filter } => {
            let mut stream = client
                .subscribe(attach_token(
                    Request::new(pb::SubscribeRequest { kinds: filter }),
                    &token,
                ))
                .await?
                .into_inner();
            while let Some(ev) = stream.message().await? {
                println!("{}", serde_json::to_string(&event_to_json(&ev))?);
            }
        }
    }
    Ok(())
}

fn event_to_json(ev: &pb::Event) -> serde_json::Value {
    use serde_json::json;
    let (kind, payload) = match &ev.payload {
        Some(pb::event::Payload::StreamState(p)) => {
            let state = pb::PlayerState::try_from(p.state)
                .unwrap_or(pb::PlayerState::Unspecified);
            (
                "stream.state",
                json!({"state": format!("{state:?}"), "url": p.url, "error": p.error}),
            )
        }
        Some(pb::event::Payload::StreamVolume(p)) => (
            "stream.volume",
            json!({"volume_percent": p.volume_percent}),
        ),
        Some(pb::event::Payload::BtDevice(p)) => (
            "bt.device",
            json!({
                "address": p.address, "name": p.name,
                "paired": p.paired, "connected": p.connected, "trusted": p.trusted,
            }),
        ),
        Some(pb::event::Payload::BtPairingRequest(p)) => (
            "bt.pairing",
            json!({"address": p.address, "passkey": p.passkey}),
        ),
        Some(pb::event::Payload::BtA2dpConnected(p)) => (
            "bt.a2dp.connected",
            json!({"address": p.address, "name": p.name}),
        ),
        Some(pb::event::Payload::BtA2dpDisconnected(p)) => (
            "bt.a2dp.disconnected",
            json!({"address": p.address, "name": p.name}),
        ),
        Some(pb::event::Payload::SystemdUnit(p)) => (
            "systemd.unit",
            json!({
                "name": p.name, "active_state": p.active_state,
                "sub_state": p.sub_state, "enabled": p.enabled,
            }),
        ),
        Some(pb::event::Payload::Volume(p)) => (
            "volume",
            json!({
                "card": p.card, "control": p.control,
                "volume_percent": p.volume_percent, "muted": p.muted,
            }),
        ),
        Some(pb::event::Payload::SnapClient(p)) => (
            "snap.client",
            json!({
                "client_id": p.client_id, "name": p.name,
                "volume_percent": p.volume_percent, "muted": p.muted,
            }),
        ),
        Some(pb::event::Payload::LibrespotState(p)) => (
            "librespot.state",
            json!({"state": p.state, "track": p.track}),
        ),
        Some(pb::event::Payload::Lagged(p)) => {
            ("lagged", json!({"dropped": p.dropped}))
        }
        None => ("unknown", json!({})),
    };
    json!({
        "timestamp_ms": ev.timestamp_ms,
        "kind": kind,
        "payload": payload,
    })
}

// --- system ----------------------------------------------------------------

async fn run_system(ep: &Endpoint, token: Option<String>, cmd: SystemCmd) -> Result<()> {
    let ch = channel(ep).await?;
    let mut client = pb::system_service_client::SystemServiceClient::new(ch);
    match cmd {
        SystemCmd::Version => {
            let r = client
                .version(attach_token(Request::new(pb::VersionRequest {}), &token))
                .await?
                .into_inner();
            print_json(&serde_json::json!({"version": r.version, "os": r.os, "arch": r.arch}))?;
        }
        SystemCmd::Health => {
            let r = client
                .health(attach_token(Request::new(pb::HealthRequest {}), &token))
                .await?
                .into_inner();
            print_json(&serde_json::json!({"ok": r.ok}))?;
        }
    }
    Ok(())
}
