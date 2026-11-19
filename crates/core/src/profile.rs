//! Profile model, TOML parsing, and validation.
//!
//! A profile file is the single source of truth for one game's bindings. Parsing
//! validates everything; an invalid file is an `Err` and never yields partial state.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Deserialize;

use crate::{
    action::{Action, ActionError},
    keys::PhysicalKey,
    window::ActiveWindow,
};

pub const LED_BLUE: u8 = 1;
pub const LED_GREEN: u8 = 2;
pub const LED_RED: u8 = 4;

/// Default LED masks by mode index: B, G, R, then combinations. Never 0: some LED is
/// always lit, so an all-dark thumb module can't be mistaken for a dead device.
pub const DEFAULT_LED_MASKS: [u8; 7] = [
    LED_BLUE,
    LED_GREEN,
    LED_RED,
    LED_BLUE | LED_GREEN,
    LED_BLUE | LED_RED,
    LED_GREEN | LED_RED,
    LED_BLUE | LED_GREEN | LED_RED,
];

pub const MAX_MODES_DEFAULT: usize = 3;
pub const MAX_MODES_EXTENDED: usize = DEFAULT_LED_MASKS.len();

pub type Layer = HashMap<PhysicalKey, Action>;

/// A compiled window-title pattern. Compared by source text.
#[derive(Clone, Debug)]
pub struct TitleRegex(regex::Regex);

impl TitleRegex {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq for TitleRegex {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for TitleRegex {}

impl<'de> Deserialize<'de> for TitleRegex {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let src = String::deserialize(d)?;
        regex::Regex::new(&src)
            .map(TitleRegex)
            .map_err(serde::de::Error::custom)
    }
}

/// Selects a profile by active window. Every field that is set must match.
#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchRule {
    /// Window class / app id, compared exactly (case-sensitive).
    pub class: Option<String>,
    /// Regex searched anywhere in the window title.
    pub title_regex: Option<TitleRegex>,
}

impl MatchRule {
    pub fn matches(&self, w: &ActiveWindow) -> bool {
        self.class.as_ref().is_none_or(|c| *c == w.class)
            && self
                .title_regex
                .as_ref()
                .is_none_or(|r| r.0.is_match(&w.title))
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Mode {
    /// Profile LED bits shown for this mode (B=1, G=2, R=4), 1..=7.
    pub led_mask: u8,
    pub bindings: Layer,
    pub hypershift: Option<Layer>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Profile {
    pub name: String,
    pub match_rules: Vec<MatchRule>,
    pub modes: Vec<Mode>,
    pub remember_mode: bool,
    /// Opt-in to more than 3 modes.
    pub extended_modes: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("profile name is empty")]
    EmptyName,
    #[error("profile defines no modes")]
    NoModes,
    #[error("`mode.{0}` is not a valid mode index")]
    BadModeIndex(String),
    #[error("mode indices must be contiguous from 0; `mode.{missing}` is missing")]
    ModeGap { missing: usize },
    #[error("`mode.{0}` must be a table")]
    ModeNotTable(usize),
    #[error("{count} modes defined; at most 3 are allowed unless `extended_modes = true`")]
    TooManyModes { count: usize },
    #[error("{count} modes defined; the maximum is 7")]
    TooManyModesExtended { count: usize },
    #[error("mode.{mode}: `led_mask` must be an integer 1..=7 (got {value})")]
    BadLedMask { mode: usize, value: String },
    #[error("mode.{mode}: led_mask {mask} is already used by another mode")]
    DuplicateLed { mode: usize, mask: u8 },
    #[error("mode.{mode}{layer}: unknown key `{name}`")]
    UnknownKey {
        mode: usize,
        layer: &'static str,
        name: String,
    },
    #[error("mode.{mode}{layer}: `{key}` is bound twice")]
    DuplicateBinding {
        mode: usize,
        layer: &'static str,
        key: String,
    },
    #[error("mode.{mode}{layer}: `{key}`: {source}")]
    Binding {
        mode: usize,
        layer: &'static str,
        key: String,
        source: ActionError,
    },
    #[error("mode.{mode}{layer}: `{key}` sets mode {target}, but the profile has {modes} modes")]
    SetModeOutOfRange {
        mode: usize,
        layer: &'static str,
        key: String,
        target: u8,
        modes: usize,
    },
    #[error(
        "[[match]] #{index} needs `class` or `title_regex`; an empty rule would match every window"
    )]
    EmptyMatchRule { index: usize },
    #[error("mode.{mode}.hypershift must be a table")]
    HypershiftNotTable { mode: usize },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    name: String,
    #[serde(default)]
    remember_mode: bool,
    #[serde(default)]
    extended_modes: bool,
    #[serde(default, rename = "match")]
    match_rules: Vec<MatchRule>,
    #[serde(default)]
    mode: BTreeMap<String, toml::Value>,
}

impl Profile {
    pub fn from_toml(src: &str) -> Result<Profile, ProfileError> {
        let raw: RawProfile = toml::from_str(src)?;
        if raw.name.trim().is_empty() {
            return Err(ProfileError::EmptyName);
        }

        // Numeric order, not the lexicographic order of the string keys.
        if let Some(index) = raw
            .match_rules
            .iter()
            .position(|r| r.class.is_none() && r.title_regex.is_none())
        {
            return Err(ProfileError::EmptyMatchRule { index });
        }

        let mut indexed = BTreeMap::new();
        for (k, v) in raw.mode {
            let i: usize = k
                .parse()
                .map_err(|_| ProfileError::BadModeIndex(k.clone()))?;
            if indexed.insert(i, v).is_some() {
                return Err(ProfileError::BadModeIndex(k));
            }
        }
        if indexed.is_empty() {
            return Err(ProfileError::NoModes);
        }
        for want in 0..indexed.len() {
            if !indexed.contains_key(&want) {
                return Err(ProfileError::ModeGap { missing: want });
            }
        }
        let count = indexed.len();
        if count > MAX_MODES_EXTENDED {
            return Err(ProfileError::TooManyModesExtended { count });
        }
        if count > MAX_MODES_DEFAULT && !raw.extended_modes {
            return Err(ProfileError::TooManyModes { count });
        }

        let mut modes = Vec::with_capacity(count);
        let mut used_masks = HashSet::new();
        for (i, value) in indexed {
            let toml::Value::Table(table) = value else {
                return Err(ProfileError::ModeNotTable(i));
            };
            let mode = parse_mode(i, table)?;
            if !used_masks.insert(mode.led_mask) {
                return Err(ProfileError::DuplicateLed {
                    mode: i,
                    mask: mode.led_mask,
                });
            }
            modes.push(mode);
        }

        let profile = Profile {
            name: raw.name,
            match_rules: raw.match_rules,
            modes,
            remember_mode: raw.remember_mode,
            extended_modes: raw.extended_modes,
        };
        profile.check_set_mode_targets()?;
        Ok(profile)
    }

    /// Whether any `[[match]]` rule selects this window. A profile with no rules never
    /// auto-matches (it can still be the default or pinned).
    pub fn matches(&self, w: &ActiveWindow) -> bool {
        self.match_rules.iter().any(|r| r.matches(w))
    }

    fn check_set_mode_targets(&self) -> Result<(), ProfileError> {
        let n = self.modes.len();
        for (mode, m) in self.modes.iter().enumerate() {
            let layers = [
                ("", Some(&m.bindings)),
                (".hypershift", m.hypershift.as_ref()),
            ];
            for (layer, map) in layers {
                for (key, action) in map.into_iter().flatten() {
                    if let Action::SetMode(t) = action
                        && usize::from(*t) >= n
                    {
                        return Err(ProfileError::SetModeOutOfRange {
                            mode,
                            layer,
                            key: key.to_string(),
                            target: *t,
                            modes: n,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

fn parse_mode(i: usize, table: toml::Table) -> Result<Mode, ProfileError> {
    let mut led_mask = None;
    let mut hypershift = None;
    let mut rest = toml::Table::new();
    for (k, v) in table {
        match k.as_str() {
            "led_mask" => {
                let mask = v
                    .as_integer()
                    .and_then(|n| u8::try_from(n).ok())
                    .filter(|n| (1..=7).contains(n))
                    .ok_or_else(|| ProfileError::BadLedMask {
                        mode: i,
                        value: v.to_string(),
                    })?;
                led_mask = Some(mask);
            }
            "hypershift" => {
                let toml::Value::Table(t) = v else {
                    return Err(ProfileError::HypershiftNotTable { mode: i });
                };
                hypershift = Some(parse_layer(i, ".hypershift", t)?);
            }
            _ => {
                rest.insert(k, v);
            }
        }
    }
    Ok(Mode {
        led_mask: led_mask.unwrap_or(DEFAULT_LED_MASKS[i]),
        bindings: parse_layer(i, "", rest)?,
        hypershift,
    })
}

fn parse_layer(
    mode: usize,
    layer: &'static str,
    table: toml::Table,
) -> Result<Layer, ProfileError> {
    let mut out = Layer::new();
    for (name, value) in table {
        let key: PhysicalKey = name.parse().map_err(|_| ProfileError::UnknownKey {
            mode,
            layer,
            name: name.clone(),
        })?;
        let action = Action::from_toml(&value).map_err(|source| ProfileError::Binding {
            mode,
            layer,
            key: name.clone(),
            source,
        })?;
        if out.insert(key, action).is_some() {
            return Err(ProfileError::DuplicateBinding {
                mode,
                layer,
                key: name,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Key, WheelDir};

    const EXAMPLE: &str = r#"
name = "Elden Ring"
remember_mode = true

[[match]]
class = "steam_app_1245620"

[mode.0]
led_mask = 1
KEY_1 = "KEY_R"
KEY_Q = { macro = [["KEY_E", 0], ["KEY_F", 50]] }
WHEEL_UP = "KEY_PAGEUP"

[mode.0.hypershift]
KEY_1 = { set_mode = 1 }

[mode.1]
led_mask = 2

[mode.2]
"#;

    #[test]
    fn parses_design_example() {
        let p = Profile::from_toml(EXAMPLE).unwrap();
        assert_eq!(p.name, "Elden Ring");
        assert!(p.remember_mode);
        assert_eq!(p.match_rules[0].class.as_deref(), Some("steam_app_1245620"));
        assert_eq!(p.modes.len(), 3);
        assert_eq!(p.modes[0].bindings.len(), 3);
        assert!(
            p.modes[0]
                .bindings
                .contains_key(&PhysicalKey::Wheel(WheelDir::Up))
        );
        assert_eq!(
            p.modes[0].hypershift.as_ref().unwrap()
                [&PhysicalKey::Key(Key::from_name("KEY_1").unwrap())],
            Action::SetMode(1)
        );
        // Mode 2 omitted led_mask: defaults to red.
        assert_eq!(p.modes[2].led_mask, LED_RED);
    }

    #[test]
    fn shipped_examples_parse() {
        let p = Profile::from_toml(include_str!("../../../docs/examples/smoke-test.toml")).unwrap();
        assert_eq!(p.modes.len(), 3);
        assert!(p.modes[0].hypershift.is_some());
    }

    fn win(class: &str, title: &str) -> ActiveWindow {
        ActiveWindow {
            class: class.into(),
            title: title.into(),
        }
    }

    #[test]
    fn match_rules() {
        let p = Profile::from_toml(
            "name='x'\n[[match]]\nclass='steam_app_1'\n[[match]]\nclass='kitty'\ntitle_regex='^vim '\n[mode.0]\n",
        )
        .unwrap();
        assert!(p.matches(&win("steam_app_1", "anything")), "class alone");
        assert!(p.matches(&win("kitty", "vim notes.md")), "class and title");
        assert!(!p.matches(&win("kitty", "zsh")), "title must match too");
        assert!(
            !p.matches(&win("Steam_App_1", "")),
            "class is case-sensitive"
        );
        let none = Profile::from_toml("name='x'\n[mode.0]\n").unwrap();
        assert!(
            !none.matches(&win("kitty", "")),
            "no rules never auto-match"
        );
    }

    #[test]
    fn title_regex_is_searched_not_anchored() {
        let p = Profile::from_toml("name='x'\n[[match]]\ntitle_regex='Elden'\n[mode.0]\n").unwrap();
        assert!(p.matches(&win("", "ELDEN RING - Elden Ring")));
    }

    #[test]
    fn bad_match_rules_rejected_at_load() {
        assert!(matches!(
            err("name='x'\n[[match]]\n[mode.0]\n"),
            ProfileError::EmptyMatchRule { index: 0 }
        ));
        assert!(matches!(
            err("name='x'\n[[match]]\nclass='a'\n[[match]]\n[mode.0]\n"),
            ProfileError::EmptyMatchRule { index: 1 }
        ));
        let e = err("name='x'\n[[match]]\ntitle_regex='('\n[mode.0]\n");
        assert!(matches!(e, ProfileError::Toml(_)), "{e}");
    }

    fn err(src: &str) -> ProfileError {
        Profile::from_toml(src).unwrap_err()
    }

    #[test]
    fn mode_count_rules() {
        let four = "name='x'\n[mode.0]\n[mode.1]\n[mode.2]\n[mode.3]\n";
        assert!(matches!(err(four), ProfileError::TooManyModes { count: 4 }));
        let ok = format!("extended_modes = true\n{four}");
        assert_eq!(
            Profile::from_toml(&ok).unwrap().modes[3].led_mask,
            LED_BLUE | LED_GREEN
        );
        let eight: String = (0..8).map(|i| format!("[mode.{i}]\n")).collect();
        assert!(matches!(
            err(&format!("name='x'\nextended_modes=true\n{eight}")),
            ProfileError::TooManyModesExtended { count: 8 }
        ));
        assert!(matches!(err("name='x'"), ProfileError::NoModes));
    }

    #[test]
    fn mode_indices_numeric_and_contiguous() {
        assert!(matches!(
            err("name='x'\n[mode.0]\n[mode.2]\n"),
            ProfileError::ModeGap { missing: 1 }
        ));
        assert!(matches!(
            err("name='x'\n[mode.a]\n"),
            ProfileError::BadModeIndex(_)
        ));
        // "10" must not sort before "2" (extended allows only 7, so test with 3 and a gap).
        assert!(matches!(
            err("name='x'\n[mode.0]\n[mode.1]\n[mode.10]\n"),
            ProfileError::ModeGap { missing: 2 }
        ));
    }

    #[test]
    fn led_mask_rules() {
        assert!(matches!(
            err("name='x'\n[mode.0]\nled_mask = 0\n"),
            ProfileError::BadLedMask { .. }
        ));
        assert!(matches!(
            err("name='x'\n[mode.0]\nled_mask = 8\n"),
            ProfileError::BadLedMask { .. }
        ));
        assert!(matches!(
            err("name='x'\n[mode.0]\nled_mask = 2\n[mode.1]\n"),
            // mode 1 defaults to green (2), colliding with mode 0.
            ProfileError::DuplicateLed { mode: 1, mask: 2 }
        ));
    }

    #[test]
    fn binding_errors() {
        assert!(matches!(
            err("name='x'\n[mode.0]\nKEY_NOPE = 'KEY_A'\n"),
            ProfileError::UnknownKey { mode: 0, .. }
        ));
        assert!(matches!(
            err("name='x'\n[mode.0.hypershift]\nKEY_A = 'KEY_NOPE'\n"),
            ProfileError::Binding {
                layer: ".hypershift",
                ..
            }
        ));
        assert!(matches!(
            err("name='x'\n[mode.0]\nKEY_A = { set_mode = 1 }\n"),
            ProfileError::SetModeOutOfRange {
                target: 1,
                modes: 1,
                ..
            }
        ));
        // Duplicate keys are rejected by the TOML parser itself.
        assert!(matches!(
            err("name='x'\n[mode.0]\nKEY_A = 'KEY_B'\nKEY_A = 'KEY_C'\n"),
            ProfileError::Toml(_)
        ));
    }

    #[test]
    fn rejects_unknown_top_level_and_empty_name() {
        assert!(matches!(
            err("name='x'\nbogus=1\n[mode.0]\n"),
            ProfileError::Toml(_)
        ));
        assert!(matches!(
            err("name=' '\n[mode.0]\n"),
            ProfileError::EmptyName
        ));
    }
}
