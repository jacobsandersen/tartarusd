//! Writing a [`Profile`] back to TOML (for the importer and, later, the GUI).
//!
//! The output is deterministic (keys sorted) and always parses back to an equal profile.
//! Hand-written formatting and comments are not preserved; this is for machine-written
//! files.

use toml::{Table, Value};

use crate::{
    action::Action,
    keys::Edge,
    lighting::{Frame, Light},
    profile::{Layer, Mode, Profile},
};

fn action_value(a: &Action) -> Value {
    match a {
        Action::Passthrough => "passthrough".into(),
        Action::Disabled => "disabled".into(),
        Action::NextMode => "next_mode".into(),
        Action::Chord(keys) => keys
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("+")
            .into(),
        Action::SetMode(n) => {
            let mut t = Table::new();
            t.insert("set_mode".into(), Value::Integer(i64::from(*n)));
            Value::Table(t)
        }
        Action::Macro(steps) => {
            let steps = steps
                .iter()
                .map(|s| {
                    let edge = match s.edge {
                        Edge::Down => "down",
                        Edge::Up => "up",
                    };
                    Value::Array(vec![
                        s.key.to_string().into(),
                        edge.into(),
                        Value::Integer(i64::from(s.delay_ms)),
                    ])
                })
                .collect();
            let mut t = Table::new();
            t.insert("macro".into(), Value::Array(steps));
            Value::Table(t)
        }
    }
}

fn layer_table(layer: &Layer) -> Table {
    layer
        .iter()
        .map(|(k, a)| (k.to_string(), action_value(a)))
        .collect()
}

fn frame_table(f: &Frame) -> Table {
    let mut t: Table = f
        .keys
        .iter()
        .map(|(k, c)| (k.to_string(), Value::String(c.to_string())))
        .collect();
    t.insert("default".into(), Value::String(f.default.to_string()));
    t
}

fn mode_table(m: &Mode) -> Table {
    let mut t = layer_table(&m.bindings);
    t.insert("led_mask".into(), Value::Integer(i64::from(m.led_mask)));
    if let Some(h) = &m.hypershift {
        t.insert("hypershift".into(), Value::Table(layer_table(h)));
    }
    for (light, frame_key, effect_key) in [
        (&m.lighting, "lighting", "effect"),
        (
            &m.hypershift_lighting,
            "hypershift_lighting",
            "hypershift_effect",
        ),
    ] {
        match light {
            Some(Light::Frame(f)) => {
                t.insert(frame_key.into(), Value::Table(frame_table(f)));
            }
            Some(Light::Effect(e)) => {
                t.insert(effect_key.into(), Value::Table(e.to_table()));
            }
            None => {}
        }
    }
    t
}

impl Profile {
    /// Serialize to TOML that [`Profile::from_toml`] parses back to an equal profile.
    pub fn to_toml(&self) -> String {
        let mut t = Table::new();
        t.insert("name".into(), Value::String(self.name.clone()));
        if self.remember_mode {
            t.insert("remember_mode".into(), Value::Boolean(true));
        }
        if self.extended_modes {
            t.insert("extended_modes".into(), Value::Boolean(true));
        }
        if !self.match_rules.is_empty() {
            let rules = self
                .match_rules
                .iter()
                .map(|r| {
                    let mut rt = Table::new();
                    if let Some(c) = &r.class {
                        rt.insert("class".into(), Value::String(c.clone()));
                    }
                    if let Some(re) = &r.title_regex {
                        rt.insert("title_regex".into(), Value::String(re.as_str().to_owned()));
                    }
                    Value::Table(rt)
                })
                .collect();
            t.insert("match".into(), Value::Array(rules));
        }
        let modes: Table = self
            .modes
            .iter()
            .enumerate()
            .map(|(i, m)| (i.to_string(), Value::Table(mode_table(m))))
            .collect();
        t.insert("mode".into(), Value::Table(modes));
        toml::to_string(&t).expect("a profile's tables always serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trips(src: &str) -> Profile {
        let p = Profile::from_toml(src).unwrap();
        let text = p.to_toml();
        let back = Profile::from_toml(&text)
            .unwrap_or_else(|e| panic!("re-parse failed: {e}\n---\n{text}"));
        assert_eq!(back, p, "round trip changed the profile:\n{text}");
        back
    }

    #[test]
    fn the_shipped_example_round_trips() {
        round_trips(include_str!("../../../docs/examples/smoke-test.toml"));
    }

    #[test]
    fn every_action_kind_and_all_options_round_trip() {
        round_trips(
            r##"
name = "Everything \"quoted\""
remember_mode = true
extended_modes = true

[[match]]
class = "steam_app_1"

[[match]]
class = "kitty"
title_regex = '^vim (.*)\.md$'

[mode.0]
led_mask = 7
KEY_1 = "KEY_A"
KEY_2 = "KEY_LEFTCTRL+KEY_LEFTSHIFT+KEY_T"
KEY_3 = "disabled"
KEY_4 = "passthrough"
KEY_5 = "next_mode"
KEY_Q = { set_mode = 2 }
KEY_W = { macro = [["KEY_LEFTSHIFT", "down", 0], ["KEY_A", 30], ["KEY_LEFTSHIFT", "up", 5]] }
WHEEL_UP = "KEY_PAGEUP"
BTN_MIDDLE = "KEY_ENTER"
[mode.0.hypershift]
KEY_1 = { set_mode = 1 }
KEY_UP = "KEY_F1"
[mode.0.lighting]
default = "#000102"
KEY_W = "#00ffff"
BTN_MIDDLE = "#ff00ff"
[mode.0.hypershift_lighting]
default = "#101010"

[mode.1]
[mode.2]
[mode.3]
"##,
        );
    }

    #[test]
    fn firmware_effects_round_trip() {
        let p = round_trips(
            "name = 'E'\n[mode.0]\n[mode.0.effect]\ntype = 'starlight'\ncolors = ['#ff00ff']\nspeed = 3\n\
             [mode.0.hypershift_effect]\ntype = 'wave'\ndirection = 'right'\n\
             [mode.1]\n[mode.1.lighting]\ndefault = '#000000'\n[mode.1.hypershift_effect]\ntype = 'spectrum'\n",
        );
        assert!(p.to_toml().contains("[mode.0.effect]"));
        assert!(p.to_toml().contains("[mode.0.hypershift_effect]"));
    }

    #[test]
    fn output_is_deterministic() {
        let p = Profile::from_toml(include_str!("../../../docs/examples/smoke-test.toml")).unwrap();
        assert_eq!(p.to_toml(), p.to_toml());
    }

    #[test]
    fn defaults_are_omitted() {
        let text = Profile::from_toml("name='x'\n[mode.0]\n")
            .unwrap()
            .to_toml();
        assert!(!text.contains("remember_mode") && !text.contains("extended_modes"));
        assert!(!text.contains("[[match]]"));
    }
}
