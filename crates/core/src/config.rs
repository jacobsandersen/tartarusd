//! Global daemon configuration (`~/.config/tartarusd/config.toml`).

use std::time::Duration;

use serde::Deserialize;

use crate::keys::Key;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub mode_key: Key,
    /// A press-and-release shorter than this (with no other input) is a tap.
    pub tap_term: Duration,
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
            tap_term: Duration::from_millis(350),
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
    #[error("`tap_term_ms` must be 1..=5000")]
    BadTapTerm,
    #[error("`hypershift_led_mask` must be 1..=7")]
    BadLedMask,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    mode_key: Option<String>,
    tap_term_ms: Option<u64>,
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
        if raw
            .hypershift_led_mask
            .is_some_and(|m| !(1..=7).contains(&m))
        {
            return Err(ConfigError::BadLedMask);
        }
        Ok(Config {
            mode_key: raw.mode_key.map_or(Ok(d.mode_key), |s| s.parse())?,
            tap_term,
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
