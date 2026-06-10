//! zerod — one binary, two modes.
//!
//! `zerod` (no args)         → run as a gRPC server (reads zerod.toml).
//! `zerod serve …`           → explicit server invocation.
//! `zerod bluetooth scan`    → client subcommands. Defaults host=localhost
//! `zerod stream play …`       port=50151; override with --host / --port and
//! `zerod systemd start …`     optional --bearer-token.
//! `zerod config get …`

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::Request;
use zerod_proto::v1alpha1 as pb;

#[derive(Parser)]
#[command(name = "zerod", version, about = "headless audio/bluetooth/systemd control daemon")]
struct Cli {
    /// Path to zerod.toml. Server mode only.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Client mode: target server host. Ignored for `serve`.
    #[arg(long, global = true, env = "ZEROD_HOST", default_value = "localhost")]
    host: String,

    /// Client mode: target server port. Ignored for `serve`.
    #[arg(long, global = true, env = "ZEROD_PORT", default_value_t = 50151)]
    port: u16,

    /// Client mode: bearer token to send.
    #[arg(long, global = true, env = "ZEROD_BEARER_TOKEN")]
    bearer_token: Option<String>,

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
        #[arg(long)]
        cpal_device: Option<String>,
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
    match cli.command {
        None | Some(Command::Serve) => run_server(cli.config.as_deref()).await,
        Some(Command::Bluetooth(cmd)) => run_bluetooth(&cli.host, cli.port, cli.bearer_token, cmd).await,
        Some(Command::Stream(cmd)) => run_stream(&cli.host, cli.port, cli.bearer_token, cmd).await,
        Some(Command::Systemd(cmd)) => run_systemd(&cli.host, cli.port, cli.bearer_token, cmd).await,
        Some(Command::Config(cmd)) => run_config(&cli.host, cli.port, cli.bearer_token, cmd).await,
        Some(Command::System(cmd)) => run_system(&cli.host, cli.port, cli.bearer_token, cmd).await,
        Some(Command::Volume(cmd)) => run_volume(&cli.host, cli.port, cli.bearer_token, cmd).await,
    }
}

async fn run_server(config: Option<&std::path::Path>) -> Result<()> {
    let settings = zerod_server::load_settings(config)?;
    zerod_server::serve(settings).await
}

// --- client helpers ---------------------------------------------------------

async fn channel(host: &str, port: u16) -> Result<Channel> {
    let url = format!("http://{host}:{port}");
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

async fn run_bluetooth(host: &str, port: u16, token: Option<String>, cmd: BluetoothCmd) -> Result<()> {
    let ch = channel(host, port).await?;
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

async fn run_stream(host: &str, port: u16, token: Option<String>, cmd: StreamCmd) -> Result<()> {
    let ch = channel(host, port).await?;
    let mut client = pb::stream_service_client::StreamServiceClient::new(ch);
    match cmd {
        StreamCmd::Play { url, output, pipe_path, cpal_device } => {
            let output = match output {
                OutputArg::Cpal => pb::AudioOutput::Cpal,
                OutputArg::Stdout => pb::AudioOutput::Stdout,
                OutputArg::Pipe => pb::AudioOutput::Pipe,
            } as i32;
            client
                .play(attach_token(
                    Request::new(pb::PlayRequest { url, output, pipe_path, cpal_device }),
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

async fn run_volume(host: &str, port: u16, token: Option<String>, cmd: VolumeCmd) -> Result<()> {
    let ch = channel(host, port).await?;
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

async fn run_systemd(host: &str, port: u16, token: Option<String>, cmd: SystemdCmd) -> Result<()> {
    let ch = channel(host, port).await?;
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

async fn run_config(host: &str, port: u16, token: Option<String>, cmd: ConfigCmd) -> Result<()> {
    let ch = channel(host, port).await?;
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

// --- system ----------------------------------------------------------------

async fn run_system(host: &str, port: u16, token: Option<String>, cmd: SystemCmd) -> Result<()> {
    let ch = channel(host, port).await?;
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
