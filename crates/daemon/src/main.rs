//! tartarusd: evdev -> state machine -> uinput remapping daemon for the Tartarus V2.

mod device;
mod emit;
mod led;
mod profiles;
mod translate;

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use clap::Parser;
use evdev::InputEvent;
use tartarusd_core::{Config, Effect, Machine, ProfileSet, profile::LED_BLUE};
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
            Effect::ShiftChanged(on) => info!(shifted = on, "hypershift"),
            Effect::RunMacro(_) => warn!("macros are not implemented yet; ignoring"),
            Effect::Key { .. } | Effect::Wheel(_) => {}
        }
    }
    Ok(())
}

/// Re-read the profiles directory and swap the active profile if its file changed.
/// Invalid files keep their previous good version (see `ProfileSet::update`).
fn reload(dir: &Path, set: &mut ProfileSet, machine: &mut Machine) -> Vec<Effect> {
    let files = match profiles::read_files(dir) {
        Ok(f) => f,
        Err(e) => {
            error!("cannot reload profiles from {}: {e}", dir.display());
            return Vec::new();
        }
    };
    for e in set.update(&files) {
        warn!(file = %e.label, "profile not reloaded: {}", e.error);
    }
    let current = machine.profile().clone();
    match set.updated(&current) {
        Some(new) => {
            info!(profile = %new.name, "profile reloaded");
            machine.swap_profile(new)
        }
        None => {
            if set.get(&current.name).is_none() {
                warn!(profile = %current.name, "active profile is no longer present; keeping the loaded version");
            }
            Vec::new()
        }
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

    let (_watcher, mut changes) = profiles::watch(&profiles_dir)
        .with_context(|| format!("watching {}", profiles_dir.display()))?;
    let mut reload_at: Option<tokio::time::Instant> = None;
    let mut machine = Machine::new(cfg, profile);
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
            Some(()) = changes.recv() => {
                // Debounce without blocking input: just move the deadline.
                reload_at = Some(tokio::time::Instant::now() + profiles::DEBOUNCE);
            }
            () = tokio::time::sleep_until(reload_at.unwrap_or_else(tokio::time::Instant::now)), if reload_at.is_some() => {
                reload_at = None;
                let fx = reload(&profiles_dir, &mut set, &mut machine);
                if let Err(err) = apply(&fx, &mut outputs, &led_tx) {
                    break 'run Err(err);
                }
            }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tartarusd_core::{Edge, Event, Key, PhysicalKey};

    use super::*;

    fn profile_text(key1: &str) -> String {
        format!("name = 'P'\n[mode.0]\nKEY_1 = '{key1}'\n[mode.1]\n")
    }

    fn setup(dir: &Path, text: &str) -> (ProfileSet, Machine) {
        std::fs::write(dir.join("p.toml"), text).unwrap();
        let mut set = ProfileSet::new();
        assert!(set.update(&profiles::read_files(dir).unwrap()).is_empty());
        let machine = Machine::new(Config::default(), set.pick(None).unwrap());
        (set, machine)
    }

    fn k(name: &str) -> PhysicalKey {
        PhysicalKey::Key(Key::from_name(name).unwrap())
    }

    #[test]
    fn valid_edit_swaps_and_releases_held_keys() {
        let d = tempfile::tempdir().unwrap();
        let (mut set, mut m) = setup(d.path(), &profile_text("KEY_A"));
        let fx = m.handle(Duration::ZERO, Event::Press(k("KEY_1")));
        assert_eq!(
            fx,
            vec![Effect::Key {
                key: Key::from_name("KEY_A").unwrap(),
                edge: Edge::Down
            }]
        );

        std::fs::write(d.path().join("p.toml"), profile_text("KEY_B")).unwrap();
        let fx = reload(d.path(), &mut set, &mut m);
        assert_eq!(
            fx[0],
            Effect::Key {
                key: Key::from_name("KEY_A").unwrap(),
                edge: Edge::Up
            }
        );
        assert!(fx.contains(&Effect::SetLed(1)));
        // New bindings are live.
        m.handle(Duration::ZERO, Event::Release(k("KEY_1")));
        let fx = m.handle(Duration::ZERO, Event::Press(k("KEY_1")));
        assert_eq!(
            fx,
            vec![Effect::Key {
                key: Key::from_name("KEY_B").unwrap(),
                edge: Edge::Down
            }]
        );
    }

    #[test]
    fn invalid_edit_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (mut set, mut m) = setup(d.path(), &profile_text("KEY_A"));
        std::fs::write(d.path().join("p.toml"), profile_text("KEY_NOPE")).unwrap();
        assert!(reload(d.path(), &mut set, &mut m).is_empty());
        let fx = m.handle(Duration::ZERO, Event::Press(k("KEY_1")));
        assert_eq!(
            fx,
            vec![Effect::Key {
                key: Key::from_name("KEY_A").unwrap(),
                edge: Edge::Down
            }]
        );
    }

    #[test]
    fn unrelated_file_change_does_not_swap() {
        let d = tempfile::tempdir().unwrap();
        let (mut set, mut m) = setup(d.path(), &profile_text("KEY_A"));
        m.handle(Duration::ZERO, Event::Press(k("KEY_1")));
        std::fs::write(d.path().join("other.toml"), "name = 'Other'\n[mode.0]\n").unwrap();
        // Held KEY_A must not be released by an unrelated profile appearing.
        assert!(reload(d.path(), &mut set, &mut m).is_empty());
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn deleted_active_profile_keeps_running() {
        let d = tempfile::tempdir().unwrap();
        let (mut set, mut m) = setup(d.path(), &profile_text("KEY_A"));
        std::fs::remove_file(d.path().join("p.toml")).unwrap();
        assert!(reload(d.path(), &mut set, &mut m).is_empty());
        assert_eq!(m.profile().name, "P");
    }
}
