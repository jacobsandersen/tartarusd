//! Global daemon configuration (`~/.config/tartarusd/config.toml`).

use std::time::Duration;

use serde::Deserialize;

use crate::keys::Key;

/// How the mode key changes modes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ModeSwitch {
    /// Pressing the key shows Hypershift at once; two quick taps in a row cycle the mode.
    #[default]
    DoubleTap,
    /// A single quick tap cycles the mode; Hypershift starts when another key is pressed
    /// or the key has been held for `tap_term`.
    Tap,
    /// Like `DoubleTap`, but two quick taps open a timed selection: the profile LED blinks,
    /// D-pad up/down choose a mode, and a single tap of the mode key picks it.
    Select,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub mode_key: Key,
    pub mode_switch: ModeSwitch,
    /// How long counts as "quick": a tap must be shorter than this, and in `double_tap`
    /// mode so must the gap between the two taps.
    pub tap_term: Duration,
    /// (`Select` only) how long the selection waits without input before it is cancelled.
    pub select_timeout: Duration,
    /// Keys unmapped in a Hypershift layer fall through to the mode's own bindings.
    pub hypershift_fallthrough: bool,
    /// LED mask shown while Hypershift is held (restored on release).
    pub hypershift_led_mask: Option<u8>,
    pub default_profile: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode_key: Key::LEFTALT,
            mode_switch: ModeSwitch::default(),
            tap_term: Duration::from_millis(350),
            select_timeout: Duration::from_millis(4000),
            hypershift_fallthrough: true,
            hypershift_led_mask: None,
            default_profile: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error(transparent)]
    UnknownKey(#[from] crate::keys::UnknownKey),
    #[error("`mode_switch` must be \"double_tap\", \"tap\" or \"select\" (got `{0}`)")]
    BadModeSwitch(String),
    #[error("`tap_term_ms` must be 1..=5000")]
    BadTapTerm,
    #[error("`select_timeout_ms` must be 500..=60000")]
    BadSelectTimeout,
    #[error("`hypershift_led_mask` must be 1..=7")]
    BadLedMask,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    mode_key: Option<String>,
    mode_switch: Option<String>,
    tap_term_ms: Option<u64>,
    select_timeout_ms: Option<u64>,
    hypershift_fallthrough: Option<bool>,
    hypershift_led_mask: Option<u8>,
    default_profile: Option<String>,
}

impl Config {
    pub fn from_toml(src: &str) -> Result<Config, ConfigError> {
        let raw: Raw = toml::from_str(src)?;
        let d = Config::default();
        let tap_term = match raw.tap_term_ms {
            Some(ms) if (1..=5000).contains(&ms) => Duration::from_millis(ms),
            Some(_) => return Err(ConfigError::BadTapTerm),
            None => d.tap_term,
        };
        let select_timeout = match raw.select_timeout_ms {
            Some(ms) if (500..=60_000).contains(&ms) => Duration::from_millis(ms),
            Some(_) => return Err(ConfigError::BadSelectTimeout),
            None => d.select_timeout,
        };
        if raw
            .hypershift_led_mask
            .is_some_and(|m| !(1..=7).contains(&m))
        {
            return Err(ConfigError::BadLedMask);
        }
        let mode_switch = match raw.mode_switch.as_deref() {
            None | Some("double_tap") => ModeSwitch::DoubleTap,
            Some("tap") => ModeSwitch::Tap,
            Some("select") => ModeSwitch::Select,
            Some(other) => return Err(ConfigError::BadModeSwitch(other.to_owned())),
        };
        Ok(Config {
            mode_switch,
            mode_key: raw.mode_key.map_or(Ok(d.mode_key), |s| s.parse())?,
            tap_term,
            select_timeout,
            hypershift_fallthrough: raw
                .hypershift_fallthrough
                .unwrap_or(d.hypershift_fallthrough),
            hypershift_led_mask: raw.hypershift_led_mask,
            default_profile: raw.default_profile,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
    }

    #[test]
    fn mode_switch_defaults_to_double_tap_and_is_configurable() {
        assert_eq!(Config::default().mode_switch, ModeSwitch::DoubleTap);
        assert_eq!(
            Config::from_toml("").unwrap().mode_switch,
            ModeSwitch::DoubleTap
        );
        let c = |v: &str| Config::from_toml(&format!("mode_switch = '{v}'"));
        assert_eq!(c("tap").unwrap().mode_switch, ModeSwitch::Tap);
        assert_eq!(c("double_tap").unwrap().mode_switch, ModeSwitch::DoubleTap);
        assert_eq!(c("select").unwrap().mode_switch, ModeSwitch::Select);
        assert!(matches!(c("triple_tap"), Err(ConfigError::BadModeSwitch(s)) if s == "triple_tap"));
    }

    #[test]
    fn select_timeout_is_configurable_and_bounded() {
        assert_eq!(
            Config::from_toml("select_timeout_ms = 2500")
                .unwrap()
                .select_timeout,
            Duration::from_millis(2500)
        );
        for bad in ["0", "499", "60001"] {
            assert!(matches!(
                Config::from_toml(&format!("select_timeout_ms = {bad}")),
                Err(ConfigError::BadSelectTimeout)
            ));
        }
    }

    #[test]
    fn overrides_and_errors() {
        let c = Config::from_toml(
            "mode_key='KEY_CAPSLOCK'\ntap_term_ms=150\nhypershift_fallthrough=false\nhypershift_led_mask=7\n",
        )
        .unwrap();
        assert_eq!(c.mode_key, Key::from_name("KEY_CAPSLOCK").unwrap());
        assert_eq!(c.tap_term, Duration::from_millis(150));
        assert!(!c.hypershift_fallthrough);
        assert_eq!(c.hypershift_led_mask, Some(7));
        assert!(matches!(
            Config::from_toml("tap_term_ms=0"),
            Err(ConfigError::BadTapTerm)
        ));
        assert!(matches!(
            Config::from_toml("hypershift_led_mask=0"),
            Err(ConfigError::BadLedMask)
        ));
        assert!(matches!(
            Config::from_toml("mode_key='nope'"),
            Err(ConfigError::UnknownKey(_))
        ));
    }
}
