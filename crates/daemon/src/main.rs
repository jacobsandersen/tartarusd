//! tartarusd: evdev -> state machine -> uinput remapping daemon for the Tartarus V2.

mod app;
mod device;
mod emit;
mod ipc;
mod led;
mod profiles;
mod translate;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, bail};
use clap::Parser;
use evdev::InputEvent;
use tartarusd_core::{Config, Machine, ProfileSet, profile::LED_BLUE};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Directory of profile `*.toml` files, watched for changes.
    /// Default: $XDG_CONFIG_HOME/tartarusd/profiles
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
    /// Profile (by its `name`) to start with. Default: `default_profile` from the
    /// config, or the only profile if there is exactly one.
    #[arg(long)]
    profile: Option<String>,
    /// Global config. Default: $XDG_CONFIG_HOME/tartarusd/config.toml (optional)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Exit (releasing the device) after this many seconds. A safety net for testing.
    #[arg(long, value_name = "SECS")]
    exit_after: Option<u64>,
}

enum Msg {
    Input(InputEvent),
    DeviceLost(String),
}

fn config_dir() -> anyhow::Result<PathBuf> {
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d).join("tartarusd"));
    }
    let home = std::env::var_os("HOME").context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(PathBuf::from(home).join(".config/tartarusd"))
}

fn load_config(path: &Path, required: bool) -> anyhow::Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(s) => Config::from_toml(&s).with_context(|| format!("{}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();

    let dir = config_dir()?;
    let cfg_path = args
        .config
        .clone()
        .unwrap_or_else(|| dir.join("config.toml"));
    let cfg = load_config(&cfg_path, args.config.is_some())?;
    let profiles_dir = args
        .profiles_dir
        .clone()
        .unwrap_or_else(|| dir.join("profiles"));
    let mut set = ProfileSet::new();
    let files = profiles::read_files(&profiles_dir)
        .with_context(|| format!("reading profiles directory {}", profiles_dir.display()))?;
    for e in set.update(&files) {
        warn!(file = %e.label, "ignoring invalid profile: {}", e.error);
    }
    let preferred = args.profile.clone().or_else(|| cfg.default_profile.clone());
    let profile = set
        .pick(preferred.as_deref())
        .with_context(|| format!("choosing a profile from {}", profiles_dir.display()))?;
    info!(profile = %profile.name, modes = profile.modes.len(), available = set.len(), "loaded profiles");

    // Everything that can fail without side effects comes before grabbing the device.
    let leds = led::RazerLeds::connect().await?;
    let outputs =
        emit::Outputs::new().context("creating uinput devices (is /dev/uinput accessible?)")?;
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<app::Command>(32);
    let conn = ipc::serve(cmd_tx).await.with_context(|| {
        format!(
            "claiming {} on the session bus (is another tartarusd running?)",
            tartarusd_ipc::BUS_NAME
        )
    })?;
    let (sig_tx, sig_rx) = mpsc::unbounded_channel();
    let signal_task = tokio::spawn(ipc::run_signals(conn.clone(), sig_rx));

    let nodes = device::find_nodes(Path::new(device::BY_ID_DIR))
        .with_context(|| format!("scanning {}", device::BY_ID_DIR))?;
    if nodes.is_empty() {
        bail!(
            "no Razer Tartarus V2 event nodes found in {}",
            device::BY_ID_DIR
        );
    }
    let mut devices = Vec::new();
    for path in &nodes {
        let mut dev =
            evdev::Device::open(path).with_context(|| format!("opening {}", path.display()))?;
        dev.grab().with_context(|| {
            format!(
                "grabbing {} (is input-remapper or another grabber running?)",
                path.display()
            )
        })?;
        info!(node = %path.display(), "grabbed");
        devices.push((path.display().to_string(), dev));
    }
    let (_watcher, mut changes) = profiles::watch(&profiles_dir)
        .with_context(|| format!("watching {}", profiles_dir.display()))?;
    let mut reload_at: Option<Instant> = None;

    let machine = Machine::new(cfg, profile);
    let (led_tx, led_rx) = watch::channel(machine.led());
    let led_task = tokio::spawn(led::run(leds, led_rx));
    // The Sink needs the outputs; the shutdown path needs the LED sender too.
    let led_tx_shutdown = led_tx.clone();
    let mut app = app::App::new(machine, set, outputs, led_tx, sig_tx);

    let (tx, mut rx) = mpsc::channel::<Msg>(256);
    let mut readers = JoinSet::new();
    for (name, dev) in devices {
        let mut stream = dev.into_event_stream()?;
        let tx = tx.clone();
        readers.spawn(async move {
            loop {
                match stream.next_event().await {
                    Ok(ev) => {
                        if tx.send(Msg::Input(ev)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::DeviceLost(format!("{name}: {e}"))).await;
                        break;
                    }
                }
            }
        });
    }
    drop(tx);

    let mut sigterm = signal(SignalKind::terminate())?;
    let timeout = async {
        match args.exit_after {
            Some(s) => tokio::time::sleep(Duration::from_secs(s)).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(timeout);

    let start = Instant::now();
    info!("running; Ctrl-C to stop");
    let result: anyhow::Result<()> = 'run: loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(Msg::Input(ev)) => {
                    if let Err(err) = app.input(start.elapsed(), &ev) {
                        break 'run Err(err);
                    }
                }
                Some(Msg::DeviceLost(why)) => {
                    error!("device lost: {why}");
                    break Err(anyhow::anyhow!("device lost: {why}"));
                }
                None => break Ok(()),
            },
            Some(cmd) = cmd_rx.recv() => {
                if let Err(err) = app.command(cmd, Instant::now()) {
                    break 'run Err(err);
                }
            }
            () = tokio::time::sleep_until(app.capture_deadline().unwrap_or_else(Instant::now)),
                if app.capture_deadline().is_some() => {
                warn!("capture timed out; resuming remapping");
                app.stop_capture();
            }
            Some(()) = changes.recv() => {
                // Debounce without blocking input: just move the deadline.
                reload_at = Some(Instant::now() + profiles::DEBOUNCE);
            }
            () = tokio::time::sleep_until(reload_at.unwrap_or_else(Instant::now)), if reload_at.is_some() => {
                reload_at = None;
                if let Err(err) = app.reload(&profiles_dir) {
                    break 'run Err(err);
                }
            }
            _ = tokio::signal::ctrl_c() => { info!("interrupted"); break Ok(()); }
            _ = sigterm.recv() => { info!("terminated"); break Ok(()); }
            () = &mut timeout => { info!("--exit-after elapsed"); break Ok(()); }
        }
    };

    // Shutdown: release every output key that is down, restore the power-on LED.
    app.shutdown();
    drop(app); // closes the signal channel
    let _ = tokio::time::timeout(Duration::from_secs(1), signal_task).await;
    // Dropping the readers closes the device fds, which releases the grabs.
    readers.abort_all();
    while readers.join_next().await.is_some() {}
    led_tx_shutdown.send_replace(LED_BLUE); // power-on default
    drop(led_tx_shutdown);
    let _ = tokio::time::timeout(Duration::from_secs(2), led_task).await;
    info!("stopped");
    result
}
