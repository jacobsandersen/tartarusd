//! tartarusd: evdev -> state machine -> uinput remapping daemon for the Tartarus V2.

mod device;
mod emit;
mod led;
mod translate;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use clap::Parser;
use evdev::InputEvent;
use tartarusd_core::{Config, Effect, Machine, Profile, profile::LED_BLUE};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::{mpsc, watch},
    task::JoinSet,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Profile file. Default: $XDG_CONFIG_HOME/tartarusd/profiles/default.toml
    #[arg(long)]
    profile: Option<PathBuf>,
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

fn apply(
    fx: &[Effect],
    outputs: &mut emit::Outputs,
    led_tx: &watch::Sender<u8>,
) -> anyhow::Result<()> {
    outputs.emit(fx).context("writing to uinput")?;
    for e in fx {
        match e {
            Effect::SetLed(mask) => {
                led_tx.send_replace(*mask);
            }
            Effect::ModeChanged(m) => info!(mode = m, "mode changed"),
            Effect::RunMacro(_) => warn!("macros are not implemented yet; ignoring"),
            Effect::Key { .. } | Effect::Wheel(_) => {}
        }
    }
    Ok(())
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
    let profile_path = args
        .profile
        .clone()
        .unwrap_or_else(|| dir.join("profiles/default.toml"));
    let profile = Profile::from_toml(
        &std::fs::read_to_string(&profile_path)
            .with_context(|| format!("reading profile {}", profile_path.display()))?,
    )
    .with_context(|| format!("{}", profile_path.display()))?;
    info!(profile = %profile.name, modes = profile.modes.len(), "loaded profile");

    // Everything that can fail without side effects comes before grabbing the device.
    let leds = led::RazerLeds::connect().await?;
    let mut outputs =
        emit::Outputs::new().context("creating uinput devices (is /dev/uinput accessible?)")?;
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

    let mut machine = Machine::new(cfg, Arc::new(profile));
    let (led_tx, led_rx) = watch::channel(machine.led());
    let led_task = tokio::spawn(led::run(leds, led_rx));

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
                    for e in translate::translate(&ev) {
                        let fx = machine.handle(start.elapsed(), e);
                        if let Err(err) = apply(&fx, &mut outputs, &led_tx) {
                            break 'run Err(err);
                        }
                    }
                }
                Some(Msg::DeviceLost(why)) => {
                    error!("device lost: {why}");
                    break Err(anyhow::anyhow!("device lost: {why}"));
                }
                None => break Ok(()),
            },
            _ = tokio::signal::ctrl_c() => { info!("interrupted"); break Ok(()); }
            _ = sigterm.recv() => { info!("terminated"); break Ok(()); }
            () = &mut timeout => { info!("--exit-after elapsed"); break Ok(()); }
        }
    };

    // Shutdown: release every output key that is down, restore the power-on LED.
    release_held(&mut machine, &mut outputs);
    finish(readers, led_tx, led_task, result).await
}

/// Best-effort release of all held output keys.
fn release_held(machine: &mut Machine, outputs: &mut emit::Outputs) {
    let fx = machine.release_all();
    if let Err(e) = outputs.emit(&fx) {
        error!("releasing held keys failed: {e}");
    }
}

async fn finish(
    mut readers: JoinSet<()>,
    led_tx: watch::Sender<u8>,
    led_task: tokio::task::JoinHandle<()>,
    result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Dropping the readers closes the device fds, which releases the grabs.
    readers.abort_all();
    while readers.join_next().await.is_some() {}
    led_tx.send_replace(LED_BLUE); // power-on default
    drop(led_tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), led_task).await;
    info!("stopped");
    result
}
