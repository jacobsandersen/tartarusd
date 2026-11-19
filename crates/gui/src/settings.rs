//! The global config file (`config.toml`): the options the settings page edits.
//!
//! Saving edits the existing file in place, so comments and options the page doesn't show
//! (`mode_key`, `default_profile`) are kept. The daemon reads this file only at startup.

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use tartarusd_core::{Config, ModeSwitch};
use toml_edit::{DocumentMut, Item, Value};

/// `$XDG_CONFIG_HOME/tartarusd/config.toml`, the same default the daemon uses.
pub fn default_path() -> anyhow::Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(
            std::env::var_os("HOME").context("neither XDG_CONFIG_HOME nor HOME is set")?,
        )
        .join(".config"),
    };
    Ok(base.join("tartarusd/config.toml"))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub mode_switch: ModeSwitch,
    pub tap_term_ms: u64,
    pub select_timeout_ms: u64,
    pub hypershift_fallthrough: bool,
    /// Profile LEDs shown during Hypershift (B=1, G=2, R=4), or none.
    pub hypershift_led_mask: Option<u8>,
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl From<&Config> for Settings {
    fn from(c: &Config) -> Settings {
        Settings {
            mode_switch: c.mode_switch,
            tap_term_ms: millis(c.tap_term),
            select_timeout_ms: millis(c.select_timeout),
            hypershift_fallthrough: c.hypershift_fallthrough,
            hypershift_led_mask: c.hypershift_led_mask,
        }
    }
}

impl Default for Settings {
    fn default() -> Settings {
        Settings::from(&Config::default())
    }
}

fn mode_switch_name(m: ModeSwitch) -> &'static str {
    match m {
        ModeSwitch::DoubleTap => "double_tap",
        ModeSwitch::Tap => "tap",
        ModeSwitch::Select => "select",
    }
}

/// The settings in `path`; a missing file gives the defaults.
pub fn load(path: &Path) -> anyhow::Result<Settings> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Settings::from(&Config::from_toml(&text)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Set `key`, keeping the comment after an existing value.
fn set(doc: &mut DocumentMut, key: &str, new: impl Into<Value>) {
    let mut new = new.into();
    if let Some(old) = doc.get(key).and_then(Item::as_value) {
        *new.decor_mut() = old.decor().clone();
    }
    doc[key] = Item::Value(new);
}

/// `existing` with `s` applied. Fails if the result wouldn't load.
pub fn apply(existing: &str, s: &Settings) -> anyhow::Result<String> {
    let mut doc: DocumentMut = existing.parse().context("config.toml is not valid TOML")?;
    let int = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
    set(&mut doc, "mode_switch", mode_switch_name(s.mode_switch));
    set(&mut doc, "tap_term_ms", int(s.tap_term_ms));
    set(&mut doc, "select_timeout_ms", int(s.select_timeout_ms));
    set(&mut doc, "hypershift_fallthrough", s.hypershift_fallthrough);
    match s.hypershift_led_mask {
        Some(m) => set(&mut doc, "hypershift_led_mask", i64::from(m)),
        None => {
            doc.remove("hypershift_led_mask");
        }
    }
    let text = doc.to_string();
    Config::from_toml(&text)?;
    Ok(text)
}

/// Write `s` into `path` atomically, creating the directory if needed.
pub fn save(path: &Path, s: &Settings) -> anyhow::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let text = apply(&existing, s)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_the_defaults() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&d.path().join("config.toml")).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn saving_keeps_comments_and_options_the_page_does_not_show() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("nested/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# my notes\nmode_key = \"KEY_CAPSLOCK\"\ndefault_profile = \"Game\"\ntap_term_ms = 200 # quick\n",
        )
        .unwrap();
        let s = Settings {
            mode_switch: ModeSwitch::Select,
            tap_term_ms: 250,
            select_timeout_ms: 3000,
            hypershift_fallthrough: false,
            hypershift_led_mask: Some(7),
        };
        save(&path, &s).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my notes") && text.contains("# quick"));
        assert!(text.contains("KEY_CAPSLOCK") && text.contains("default_profile"));
        assert_eq!(load(&path).unwrap(), s);
        let cfg = Config::from_toml(&text).unwrap();
        assert_eq!(cfg.default_profile.as_deref(), Some("Game"));
        assert_eq!(cfg.mode_key.to_string(), "KEY_CAPSLOCK");
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn turning_the_hypershift_leds_off_removes_the_option() {
        let text = apply(
            "hypershift_led_mask = 3\n",
            &Settings {
                hypershift_led_mask: None,
                ..Settings::default()
            },
        )
        .unwrap();
        assert!(!text.contains("hypershift_led_mask"));
    }

    #[test]
    fn out_of_range_values_are_refused_and_nothing_is_written() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        let bad = Settings {
            tap_term_ms: 0,
            ..Settings::default()
        };
        assert!(save(&path, &bad).is_err());
        assert!(!path.exists());
        assert!(
            apply(
                "",
                &Settings {
                    select_timeout_ms: 10,
                    ..Settings::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn an_invalid_existing_file_is_not_overwritten() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        std::fs::write(&path, "mode_switch = [").unwrap();
        assert!(save(&path, &Settings::default()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mode_switch = [");
    }
}
