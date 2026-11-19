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
    /// Show the active profile, mode and pin state.
    State,
    /// List loaded profiles.
    List,
    /// Pin a profile by name.
    Activate { name: String },
    /// Remove the pin.
    Unpin,
    /// Switch to a mode (0-based).
    Mode { index: u8 },
    /// Check a profile file. Works without the daemon.
    Validate { file: PathBuf },
    /// Print daemon events as they happen.
    Monitor,
    /// Convert an input-remapper preset into a profile. Works without the daemon.
    /// Mappings that can't be converted are listed on stderr.
    Import {
        /// Preset file (JSON).
        preset: PathBuf,
        /// Profile name (default: the file name).
        #[arg(long)]
        name: Option<String>,
        /// Number of modes (1-3). The preset fills mode 0.
        #[arg(long, default_value_t = 1)]
        modes: usize,
        /// Hypershift key used in the preset.
        #[arg(long, default_value = "KEY_LEFTALT")]
        mode_key: String,
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Overwrite --out if it exists.
        #[arg(long)]
        force: bool,
    },
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
        return anyhow::anyhow!("tartarusd is not running");
    }
    match &e {
        zbus::Error::MethodError(_, Some(detail), _) => anyhow::anyhow!("{detail}"),
        _ => anyhow::Error::new(e),
    }
}

/// Convert preset JSON to profile TOML. Returns the TOML, a summary line, and the warnings.
fn import_text(
    json: &str,
    name: &str,
    modes: usize,
    mode_key: &str,
) -> anyhow::Result<(String, String, Vec<String>)> {
    let mode_key = mode_key
        .parse::<tartarusd_core::Key>()
        .map_err(|e| anyhow::anyhow!("--mode-key: {e}"))?;
    let opts = tartarusd_core::import::ImportOptions {
        modes,
        mode_key,
        ..tartarusd_core::import::ImportOptions::new(name)
    };
    let imported =
        tartarusd_core::import::import_input_remapper(json, &opts).map_err(|e| match e {
            tartarusd_core::import::ImportError::NothingConverted(w) => {
                anyhow::anyhow!("nothing could be converted:\n  {}", w.join("\n  "))
            }
            other => anyhow::Error::new(other),
        })?;
    let toml = imported.profile.to_toml();
    // The writer and parser must agree; if they ever don't, fail loudly rather than
    // hand over a file the daemon would reject.
    Profile::from_toml(&toml).context("internal error: the generated profile does not validate")?;
    let summary = format!(
        "imported {} mapping(s) as \"{}\"; skipped {}",
        imported.converted,
        imported.profile.name,
        imported.warnings.len()
    );
    Ok((toml, summary, imported.warnings))
}

/// Write via a temporary sibling and `rename(2)`, so a watching daemon never sees a
/// half-written profile (the temp name does not end in `.toml`).
fn write_atomic(path: &std::path::Path, text: &str) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let file = path.file_name().context("--out must name a file")?;
    let tmp = dir.join(format!(".{}.tmp", file.to_string_lossy()));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

async fn run(cmd: Cmd) -> anyhow::Result<()> {
    if let Cmd::Import {
        preset,
        name,
        modes,
        mode_key,
        out,
        force,
    } = &cmd
    {
        let json = std::fs::read_to_string(preset)
            .with_context(|| format!("reading {}", preset.display()))?;
        let name = name.clone().unwrap_or_else(|| {
            preset
                .file_stem()
                .map_or_else(|| "Imported".into(), |s| s.to_string_lossy().into_owned())
        });
        let (toml, summary, warnings) = import_text(&json, &name, *modes, mode_key)
            .with_context(|| format!("importing {}", preset.display()))?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        match out {
            Some(path) => {
                if path.exists() && !*force {
                    anyhow::bail!("{} exists; use --force to overwrite", path.display());
                }
                write_atomic(path, &toml)?;
                eprintln!("{summary}\nwrote {}", path.display());
            }
            None => {
                print!("{toml}");
                eprintln!("{summary}");
            }
        }
        return Ok(());
    }

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
        Cmd::Validate { .. } | Cmd::Import { .. } => unreachable!("handled above"),
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

    const PRESET: &str = r#"[
      {"input_combination":[{"type":1,"code":2}],"output_symbol":"a","name":"01 (a)","mapping_type":"key_macro"},
      {"input_combination":[{"type":1,"code":56},{"type":1,"code":2}],"output_symbol":"Shift_L+1","name":"Hyper+01","mapping_type":"key_macro"},
      {"input_combination":[{"type":1,"code":3}],"output_symbol":"nonsense","name":"02","mapping_type":"key_macro"}
    ]"#;

    #[test]
    fn import_produces_a_valid_profile_and_reports_skips() {
        let (toml, summary, warnings) = import_text(PRESET, "Mine", 1, "KEY_LEFTALT").unwrap();
        let p = Profile::from_toml(&toml).unwrap();
        assert_eq!(p.name, "Mine");
        assert_eq!(p.modes[0].bindings.len(), 1);
        assert_eq!(p.modes[0].hypershift.as_ref().unwrap().len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("\"02\"") && warnings[0].contains("nonsense"));
        assert_eq!(summary, "imported 2 mapping(s) as \"Mine\"; skipped 1");
    }

    #[test]
    fn import_errors_are_clear() {
        assert!(
            import_text(PRESET, "x", 1, "KEY_NOPE")
                .unwrap_err()
                .to_string()
                .contains("--mode-key")
        );
        assert!(
            import_text(PRESET, "x", 9, "KEY_LEFTALT")
                .unwrap_err()
                .to_string()
                .contains("modes")
        );
        let e = import_text("[]", "x", 1, "KEY_LEFTALT")
            .unwrap_err()
            .to_string();
        assert!(e.contains("nothing could be converted"), "{e}");
    }

    #[test]
    fn write_atomic_replaces_without_leaving_a_temp_file() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("p.toml");
        write_atomic(&path, "one").unwrap();
        write_atomic(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["p.toml"]);
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
        assert!(ok(&["import", "p.json"]));
        assert!(ok(&[
            "import", "p.json", "--name", "X", "--modes", "3", "--out", "o.toml", "--force"
        ]));
        assert!(!ok(&["import"]));
        assert!(!ok(&["mode", "300"]), "mode index is a u8");
        assert!(!ok(&["activate"]));
    }
}
