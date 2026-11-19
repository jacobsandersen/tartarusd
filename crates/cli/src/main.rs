//! tartarusctl: command-line client for the tartarusd daemon.

use std::{path::PathBuf, process::ExitCode};

use anyhow::Context;
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use tartarusd_core::Profile;
use tartarusd_ipc::DaemonProxy;
use zbus::Connection;

#[derive(Parser)]
#[command(version, about = "Control the tartarusd daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the active profile, mode, and whether it is pinned.
    State,
    /// List loaded profiles.
    List,
    /// Pin a profile by name.
    Activate { name: String },
    /// Unpin and return to the default profile.
    Unpin,
    /// Jump to a mode (0-based index).
    Mode { index: u8 },
    /// Check a profile file for errors (does not need the daemon).
    Validate { file: PathBuf },
    /// Print daemon signals (profile/mode/shift changes, captured keys) as they happen.
    Monitor,
}

/// Validate profile text; `Ok` carries a one-line summary.
fn validate_text(text: &str) -> Result<String, String> {
    let p = Profile::from_toml(text).map_err(|e| e.to_string())?;
    let n = p.modes.len();
    Ok(format!(
        "OK: \"{}\" ({n} mode{})",
        p.name,
        if n == 1 { "" } else { "s" }
    ))
}

fn describe(e: zbus::Error) -> anyhow::Error {
    let not_running = match &e {
        zbus::Error::MethodError(name, ..) => {
            name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
        }
        zbus::Error::FDO(f) => matches!(**f, zbus::fdo::Error::ServiceUnknown(_)),
        _ => false,
    };
    if not_running {
        return anyhow::anyhow!(
            "tartarusd is not running ({} is not on the session bus)",
            tartarusd_ipc::BUS_NAME
        );
    }
    match &e {
        zbus::Error::MethodError(_, Some(detail), _) => anyhow::anyhow!("{detail}"),
        _ => anyhow::Error::new(e),
    }
}

async fn run(cmd: Cmd) -> anyhow::Result<()> {
    if let Cmd::Validate { file } = &cmd {
        let text =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        return match validate_text(&text) {
            Ok(summary) => {
                println!("{summary}");
                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!("{}: {e}", file.display())),
        };
    }

    let conn = Connection::session()
        .await
        .context("connecting to the session bus")?;
    let daemon = DaemonProxy::new(&conn).await.map_err(describe)?;
    match cmd {
        Cmd::State => {
            let (profile, mode, pinned) = daemon.get_state().await.map_err(describe)?;
            println!(
                "profile: {profile}\nmode: {mode}\npinned: {}",
                if pinned { "yes" } else { "no" }
            );
        }
        Cmd::List => {
            for name in daemon.list_profiles().await.map_err(describe)? {
                println!("{name}");
            }
        }
        Cmd::Activate { name } => daemon.activate_profile(&name).await.map_err(describe)?,
        Cmd::Unpin => daemon.unpin().await.map_err(describe)?,
        Cmd::Mode { index } => daemon.set_mode(index).await.map_err(describe)?,
        Cmd::Monitor => {
            let mut profile = daemon.receive_profile_changed().await?;
            let mut mode = daemon.receive_mode_changed().await?;
            let mut shift = daemon.receive_shift_changed().await?;
            let mut keys = daemon.receive_key_captured().await?;
            loop {
                tokio::select! {
                    Some(s) = profile.next() => println!("profile {}", s.args()?.name),
                    Some(s) = mode.next() => println!("mode {}", s.args()?.mode),
                    Some(s) = shift.next() => println!("shift {}", s.args()?.shifted),
                    Some(s) = keys.next() => println!("key {}", s.args()?.key),
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                }
            }
        }
        Cmd::Validate { .. } => unreachable!("handled above"),
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run(Cli::parse().cmd).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tartarusctl: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_the_shipped_example() {
        let text = include_str!("../../../docs/examples/smoke-test.toml");
        assert_eq!(validate_text(text).unwrap(), "OK: \"Smoke test\" (3 modes)");
    }

    #[test]
    fn validate_reports_the_first_error() {
        let err = validate_text("name = 'x'\n[mode.0]\nKEY_NOPE = 'KEY_A'\n").unwrap_err();
        assert!(err.contains("KEY_NOPE"), "{err}");
    }

    #[test]
    fn subcommands_parse() {
        let ok = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("tartarusctl").chain(args.iter().copied())).is_ok()
        };
        assert!(ok(&["state"]));
        assert!(ok(&["activate", "Elden Ring"]));
        assert!(ok(&["mode", "2"]));
        assert!(ok(&["validate", "x.toml"]));
        assert!(!ok(&["mode", "300"]), "mode index is a u8");
        assert!(!ok(&["activate"]));
    }
}
