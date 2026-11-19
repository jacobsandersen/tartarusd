//! Importing an input-remapper (2.x) preset as a tartarusd profile.
//!
//! A preset is a JSON array of mappings (see docs/hardware.md, "input-remapper preset
//! schema"). Only the forms confirmed in real presets are converted: single-key mappings
//! become base-layer bindings, and `[mode key, key]` combinations become the mode-0
//! Hypershift layer. Everything else is **reported and skipped, never guessed**.

use std::collections::HashMap;

use serde::Deserialize;

use crate::{
    action::Action,
    keys::{Key, PhysicalKey},
    profile::{DEFAULT_LED_MASKS, Layer, MAX_MODES_DEFAULT, MatchRule, Mode, Profile},
};

const EV_KEY: u32 = 1;

#[derive(Debug)]
pub struct ImportOptions {
    pub name: String,
    /// Number of modes to create. The preset's bindings go into mode 0; the others start
    /// empty (everything passes through). 1..=3.
    pub modes: usize,
    /// The code input-remapper used for Hypershift (`KEY_LEFTALT`, 56, on the Tartarus).
    pub mode_key: Key,
}

impl ImportOptions {
    pub fn new(name: impl Into<String>) -> ImportOptions {
        ImportOptions {
            name: name.into(),
            modes: 1,
            mode_key: Key::LEFTALT,
        }
    }
}

#[derive(Debug)]
pub struct Imported {
    pub profile: Profile,
    /// Mappings that became bindings.
    pub converted: usize,
    /// One line per mapping that was skipped, and why.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("not an input-remapper preset: {0}")]
    Json(#[from] serde_json::Error),
    #[error("profile name is empty")]
    EmptyName,
    #[error("`modes` must be 1..={MAX_MODES_DEFAULT}")]
    BadModes,
    #[error("nothing could be converted ({} mapping(s) skipped; see warnings)", .0.len())]
    NothingConverted(Vec<String>),
}

#[derive(Deserialize)]
struct Input {
    #[serde(rename = "type")]
    kind: u32,
    code: u32,
}

#[derive(Deserialize)]
struct Mapping {
    input_combination: Vec<Input>,
    output_symbol: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    mapping_type: String,
}

/// X keysym names (as input-remapper uses) that are not a single letter or digit,
/// mapped onto a US-layout physical key.
const KEYSYMS: &[(&str, &str)] = &[
    ("space", "KEY_SPACE"),
    ("Escape", "KEY_ESC"),
    ("Tab", "KEY_TAB"),
    ("Return", "KEY_ENTER"),
    ("BackSpace", "KEY_BACKSPACE"),
    ("Delete", "KEY_DELETE"),
    ("Insert", "KEY_INSERT"),
    ("Home", "KEY_HOME"),
    ("End", "KEY_END"),
    ("Prior", "KEY_PAGEUP"),
    ("Page_Up", "KEY_PAGEUP"),
    ("Next", "KEY_PAGEDOWN"),
    ("Page_Down", "KEY_PAGEDOWN"),
    ("Up", "KEY_UP"),
    ("Down", "KEY_DOWN"),
    ("Left", "KEY_LEFT"),
    ("Right", "KEY_RIGHT"),
    ("Shift_L", "KEY_LEFTSHIFT"),
    ("Shift_R", "KEY_RIGHTSHIFT"),
    ("Control_L", "KEY_LEFTCTRL"),
    ("Control_R", "KEY_RIGHTCTRL"),
    ("Alt_L", "KEY_LEFTALT"),
    ("Alt_R", "KEY_RIGHTALT"),
    ("Super_L", "KEY_LEFTMETA"),
    ("Super_R", "KEY_RIGHTMETA"),
    ("Caps_Lock", "KEY_CAPSLOCK"),
    ("Num_Lock", "KEY_NUMLOCK"),
    ("Scroll_Lock", "KEY_SCROLLLOCK"),
    ("Print", "KEY_SYSRQ"),
    ("Pause", "KEY_PAUSE"),
    ("Menu", "KEY_COMPOSE"),
    ("minus", "KEY_MINUS"),
    ("equal", "KEY_EQUAL"),
    ("bracketleft", "KEY_LEFTBRACE"),
    ("bracketright", "KEY_RIGHTBRACE"),
    ("backslash", "KEY_BACKSLASH"),
    ("semicolon", "KEY_SEMICOLON"),
    ("apostrophe", "KEY_APOSTROPHE"),
    ("grave", "KEY_GRAVE"),
    ("comma", "KEY_COMMA"),
    ("period", "KEY_DOT"),
    ("slash", "KEY_SLASH"),
    ("KP_Enter", "KEY_KPENTER"),
    ("KP_Add", "KEY_KPPLUS"),
    ("KP_Subtract", "KEY_KPMINUS"),
    ("KP_Multiply", "KEY_KPASTERISK"),
    ("KP_Divide", "KEY_KPSLASH"),
    ("KP_Decimal", "KEY_KPDOT"),
];

/// Resolve one symbol to a key. Plain letters and digits and the names above assume a US
/// layout (input-remapper itself resolves through your active layout). `KEY_*` / `BTN_*`
/// names are taken as-is.
fn symbol_key(sym: &str) -> Result<Key, String> {
    if let Some(k) = Key::from_name(sym) {
        return Ok(k);
    }
    let mut chars = sym.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            let name = format!("KEY_{}", c.to_ascii_uppercase());
            return Key::from_name(&name).ok_or_else(|| format!("unknown symbol `{sym}`"));
        }
        if c.is_ascii_uppercase() {
            return Err(format!(
                "`{sym}` is an uppercase keysym, which implies Shift; write Shift_L+{} instead",
                c.to_ascii_lowercase()
            ));
        }
    }
    if let Some(n) = sym.strip_prefix("KP_").and_then(|d| d.parse::<u8>().ok())
        && n <= 9
    {
        return Key::from_name(&format!("KEY_KP{n}"))
            .ok_or_else(|| format!("unknown symbol `{sym}`"));
    }
    if let Some(n) = sym.strip_prefix('F').and_then(|d| d.parse::<u8>().ok())
        && (1..=24).contains(&n)
    {
        return Key::from_name(&format!("KEY_F{n}"))
            .ok_or_else(|| format!("unknown symbol `{sym}`"));
    }
    KEYSYMS
        .iter()
        .find(|(name, _)| *name == sym)
        .and_then(|(_, key)| Key::from_name(key))
        .ok_or_else(|| format!("unknown symbol `{sym}`"))
}

/// `disable`, a single symbol, or `A+B+...` (pressed together).
fn output_action(symbol: &str) -> Result<Action, String> {
    let symbol = symbol.trim();
    if symbol == "disable" {
        return Ok(Action::Disabled);
    }
    if symbol.contains('(') {
        return Err(
            "input-remapper macro syntax is not supported; recreate it as a tartarusd macro".into(),
        );
    }
    let keys = symbol
        .split('+')
        .map(|part| symbol_key(part.trim()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Action::Chord(keys))
}

fn physical(input: &Input) -> Result<Key, String> {
    if input.kind != EV_KEY {
        return Err(format!(
            "input type {} (only key inputs are supported)",
            input.kind
        ));
    }
    let key = u16::try_from(input.code)
        .map(Key)
        .map_err(|_| format!("key code {} out of range", input.code))?;
    key.name()
        .map(|_| key)
        .ok_or_else(|| format!("unknown key code {}", input.code))
}

/// Convert a preset. Mappings that can't be converted are listed in
/// [`Imported::warnings`]; the call fails only if the JSON is not a preset or nothing at
/// all converts.
pub fn import_input_remapper(json: &str, opts: &ImportOptions) -> Result<Imported, ImportError> {
    if opts.name.trim().is_empty() {
        return Err(ImportError::EmptyName);
    }
    if !(1..=MAX_MODES_DEFAULT).contains(&opts.modes) {
        return Err(ImportError::BadModes);
    }
    let mappings: Vec<Mapping> = serde_json::from_str(json)?;

    let mut base = Layer::new();
    let mut hyper = Layer::new();
    let mut warnings = Vec::new();
    let mut converted = 0;

    for m in &mappings {
        let label = if m.name.is_empty() {
            "(unnamed mapping)"
        } else {
            m.name.as_str()
        };
        let skip = |why: &str| format!("skipped \"{label}\": {why}");

        if !(m.mapping_type.is_empty() || m.mapping_type == "key_macro") {
            warnings.push(skip(&format!(
                "mapping type `{}` is not supported",
                m.mapping_type
            )));
            continue;
        }
        let Some(symbol) = m.output_symbol.as_deref().filter(|s| !s.is_empty()) else {
            warnings.push(skip("no output symbol"));
            continue;
        };
        let (layer, input) = match m.input_combination.as_slice() {
            [one] => (&mut base, one),
            [first, second] if physical(first).is_ok_and(|k| k == opts.mode_key) => {
                (&mut hyper, second)
            }
            other => {
                warnings.push(skip(&format!(
                    "a combination of {} inputs is not supported (only a single key, or the mode key plus one key)",
                    other.len()
                )));
                continue;
            }
        };
        let key = match physical(input) {
            Ok(k) => PhysicalKey::Key(k),
            Err(why) => {
                warnings.push(skip(&why));
                continue;
            }
        };
        let action = match output_action(symbol) {
            Ok(a) => a,
            Err(why) => {
                warnings.push(skip(&why));
                continue;
            }
        };
        if layer.contains_key(&key) {
            warnings.push(skip(&format!("{key} is already bound; keeping the first")));
            continue;
        }
        layer.insert(key, action);
        converted += 1;
    }

    if converted == 0 {
        return Err(ImportError::NothingConverted(warnings));
    }

    let modes = (0..opts.modes)
        .map(|i| Mode {
            led_mask: DEFAULT_LED_MASKS[i],
            bindings: if i == 0 {
                std::mem::take(&mut base)
            } else {
                HashMap::new()
            },
            hypershift: (i == 0 && !hyper.is_empty()).then(|| std::mem::take(&mut hyper)),
            lighting: None,
            hypershift_lighting: None,
        })
        .collect();
    Ok(Imported {
        profile: Profile {
            name: opts.name.clone(),
            match_rules: Vec::<MatchRule>::new(),
            modes,
            remember_mode: false,
            extended_modes: false,
        },
        converted,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: &str) -> Key {
        Key::from_name(n).unwrap()
    }
    fn pk(n: &str) -> PhysicalKey {
        PhysicalKey::Key(k(n))
    }
    fn chord(names: &[&str]) -> Action {
        Action::Chord(names.iter().map(|n| k(n)).collect())
    }

    const REAL: &str = include_str!("../../../docs/inputremapper-tartarus-sample-real.json");
    const DISABLED: &str = include_str!("../../../docs/inputremapper-tartarus-sample.json");

    fn mapping(combo: &str, symbol: &str) -> String {
        format!(
            r#"[{{"input_combination": {combo}, "target_uinput": "keyboard", "output_symbol": "{symbol}", "name": "m", "mapping_type": "key_macro"}}]"#
        )
    }

    fn import(json: &str) -> Result<Imported, ImportError> {
        import_input_remapper(json, &ImportOptions::new("Imported"))
    }

    #[test]
    fn symbol_resolution() {
        let ok = |s: &str, want: &str| assert_eq!(symbol_key(s), Ok(k(want)), "{s}");
        ok("w", "KEY_W");
        ok("0", "KEY_0");
        ok("space", "KEY_SPACE");
        ok("Escape", "KEY_ESC");
        ok("Shift_L", "KEY_LEFTSHIFT");
        ok("KEY_EQUAL", "KEY_EQUAL"); // KEY_ names pass through
        ok("BTN_LEFT", "BTN_LEFT");
        ok("F5", "KEY_F5");
        ok("KP_3", "KEY_KP3");
        ok("Up", "KEY_UP");
        ok("period", "KEY_DOT");
        // Every table entry must resolve to a key our daemon knows.
        for (sym, _) in KEYSYMS {
            assert!(symbol_key(sym).is_ok(), "{sym}");
        }
        assert!(symbol_key("A").unwrap_err().contains("Shift_L+a"));
        assert!(symbol_key("10").is_err());
        assert!(symbol_key("nonsense").is_err());
    }

    #[test]
    fn output_forms() {
        assert_eq!(output_action("disable"), Ok(Action::Disabled));
        assert_eq!(
            output_action("Shift_L+1"),
            Ok(chord(&["KEY_LEFTSHIFT", "KEY_1"]))
        );
        assert_eq!(
            output_action("Shift_L+KEY_MINUS"),
            Ok(chord(&["KEY_LEFTSHIFT", "KEY_MINUS"]))
        );
        assert!(
            output_action("key(a).wait(10)")
                .unwrap_err()
                .contains("macro")
        );
        assert!(output_action("Shift_L+bogus").is_err());
    }

    #[test]
    fn imports_the_real_preset() {
        let r = import(REAL).unwrap();
        let mode = &r.profile.modes[0];
        // 20 keys + the 4 D-pad directions, and Hyper+01..19.
        assert_eq!(mode.bindings.len(), 24);
        let hyper = mode.hypershift.as_ref().unwrap();
        // One Hypershift mapping is skipped: "Hyper+10" outputs `Shift_L+10`, and `10` is
        // not a keysym. It is reported, not guessed.
        assert_eq!(hyper.len(), 18);
        assert_eq!(r.converted, 24 + 18);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("Hyper+10") && r.warnings[0].contains("`10`"));
    }

    #[test]
    fn spot_checks_match_the_presets_labels() {
        let r = import(REAL).unwrap();
        let m = &r.profile.modes[0];
        let b = |code_name: &str| m.bindings[&pk(code_name)].clone();
        // Keys by physical position (docs/hardware.md): 13 is KEY_S -> "w"; 12 is KEY_A -> "a".
        assert_eq!(b("KEY_S"), chord(&["KEY_W"]), "13 (w)");
        assert_eq!(b("KEY_A"), chord(&["KEY_A"]), "12 (a)");
        assert_eq!(b("KEY_X"), chord(&["KEY_S"]), "18 (s)");
        assert_eq!(b("KEY_1"), chord(&["KEY_1"]), "01 (1)");
        assert_eq!(b("KEY_5"), chord(&["KEY_5"]), "05 (5)");
        assert_eq!(b("KEY_TAB"), chord(&["KEY_6"]), "06 (6)");
        assert_eq!(b("KEY_F"), chord(&["KEY_EQUAL"]), "15 (=)");
        assert_eq!(b("KEY_CAPSLOCK"), chord(&["KEY_MINUS"]), "11 (-)");
        assert_eq!(b("KEY_LEFTSHIFT"), chord(&["KEY_M"]), "16 (m)");
        assert_eq!(b("KEY_SPACE"), chord(&["KEY_SPACE"]), "20 (space)");
        assert_eq!(b("KEY_LEFT"), chord(&["KEY_ESC"]), "D-pad left (Esc)");
        assert_eq!(
            b("KEY_RIGHT"),
            chord(&["KEY_LEFTSHIFT"]),
            "D-pad right (Shift)"
        );
        assert_eq!(b("KEY_UP"), chord(&["KEY_TAB"]), "D-pad up (Tab)");
        assert_eq!(b("KEY_DOWN"), Action::Disabled, "D-pad down");

        let h = m.hypershift.as_ref().unwrap();
        assert_eq!(
            h[&pk("KEY_1")],
            chord(&["KEY_LEFTSHIFT", "KEY_1"]),
            "Hyper+01"
        );
        assert_eq!(
            h[&pk("KEY_CAPSLOCK")],
            chord(&["KEY_LEFTSHIFT", "KEY_MINUS"]),
            "Hyper+11"
        );
        assert_eq!(h[&pk("KEY_A")], Action::Disabled, "Hyper+12");
        assert_eq!(h[&pk("KEY_LEFTSHIFT")], chord(&["KEY_P"]), "Hyper+16 (p)");
        assert_eq!(
            h[&pk("KEY_F")],
            chord(&["KEY_LEFTSHIFT", "KEY_EQUAL"]),
            "Hyper+15"
        );
        assert_eq!(b("KEY_Q"), chord(&["KEY_7"]), "07 (7)");
        // Hyper+10 is the unconvertible one (`Shift_L+10`): absent, not guessed. Key 10 is KEY_R.
        assert!(!h.contains_key(&pk("KEY_R")));
    }

    #[test]
    fn imports_the_all_disabled_preset() {
        let r = import(DISABLED).unwrap();
        assert_eq!(r.warnings, Vec::<String>::new());
        assert_eq!(r.converted, 24 + 19);
        let m = &r.profile.modes[0];
        assert!(m.bindings.values().all(|a| *a == Action::Disabled));
        assert!(
            m.hypershift
                .as_ref()
                .unwrap()
                .values()
                .all(|a| *a == Action::Disabled)
        );
    }

    #[test]
    fn result_serializes_and_reparses_to_the_same_profile() {
        for json in [REAL, DISABLED] {
            let r = import(json).unwrap();
            let text = r.profile.to_toml();
            assert_eq!(Profile::from_toml(&text).unwrap(), r.profile, "{text}");
        }
    }

    #[test]
    fn extra_modes_start_empty_with_default_leds() {
        let opts = ImportOptions {
            modes: 3,
            ..ImportOptions::new("x")
        };
        let p = import_input_remapper(&mapping(r#"[{"type":1,"code":2}]"#, "a"), &opts)
            .unwrap()
            .profile;
        assert_eq!(p.modes.len(), 3);
        assert!(p.modes[1].bindings.is_empty() && p.modes[2].bindings.is_empty());
        assert_eq!(
            p.modes.iter().map(|m| m.led_mask).collect::<Vec<_>>(),
            [1, 2, 4]
        );
        for modes in [0, 4] {
            let o = ImportOptions {
                modes,
                ..ImportOptions::new("x")
            };
            assert!(matches!(
                import_input_remapper("[]", &o),
                Err(ImportError::BadModes)
            ));
        }
    }

    #[test]
    fn unsupported_things_are_reported_not_guessed() {
        let w = |json: String| {
            let both = format!(
                "[{},{}]",
                &mapping(r#"[{"type":1,"code":2}]"#, "a")
                    [1..mapping(r#"[{"type":1,"code":2}]"#, "a").len() - 1],
                &json[1..json.len() - 1]
            );
            import(&both).unwrap().warnings
        };
        // Wheel / relative input.
        assert!(w(mapping(r#"[{"type":2,"code":8}]"#, "a"))[0].contains("input type 2"));
        // A chord of two ordinary keys.
        assert!(
            w(mapping(r#"[{"type":1,"code":2},{"type":1,"code":3}]"#, "a"))[0]
                .contains("combination")
        );
        // Mode key + key + key.
        assert!(
            w(mapping(
                r#"[{"type":1,"code":56},{"type":1,"code":2},{"type":1,"code":3}]"#,
                "a"
            ))[0]
                .contains("combination")
        );
        // input-remapper macro syntax.
        assert!(w(mapping(r#"[{"type":1,"code":3}]"#, "key(a)"))[0].contains("macro"));
        // Another mapping type.
        let analog = r#"[{"input_combination":[{"type":1,"code":4}],"output_symbol":"a","mapping_type":"analog","name":"n"}]"#;
        assert!(w(analog.to_string())[0].contains("mapping type `analog`"));
        // Unknown key code.
        assert!(w(mapping(r#"[{"type":1,"code":65000}]"#, "a"))[0].contains("unknown key code"));
        // Duplicate binding keeps the first.
        assert!(w(mapping(r#"[{"type":1,"code":2}]"#, "b"))[0].contains("already bound"));
    }

    #[test]
    fn hypershift_uses_the_configured_mode_key() {
        // Mode key KEY_CAPSLOCK (58): [58, 2] is Hypershift; [56, 2] no longer is.
        let json = format!(
            "[{},{}]",
            r#"{"input_combination":[{"type":1,"code":58},{"type":1,"code":2}],"output_symbol":"a","name":"h"}"#,
            r#"{"input_combination":[{"type":1,"code":3}],"output_symbol":"b","name":"b"}"#
        );
        let opts = ImportOptions {
            mode_key: k("KEY_CAPSLOCK"),
            ..ImportOptions::new("x")
        };
        let r = import_input_remapper(&json, &opts).unwrap();
        assert_eq!(r.profile.modes[0].hypershift.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn errors() {
        assert!(matches!(import("{}"), Err(ImportError::Json(_))));
        assert!(matches!(import("not json"), Err(ImportError::Json(_))));
        assert!(matches!(
            import("[]"),
            Err(ImportError::NothingConverted(_))
        ));
        assert!(matches!(
            import_input_remapper("[]", &ImportOptions::new("  ")),
            Err(ImportError::EmptyName)
        ));
    }
}
