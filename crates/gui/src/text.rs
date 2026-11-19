//! Text forms the editor shows and accepts: short descriptions of actions for the key
//! caps, chord text, and the one-step-per-line macro format.

use tartarusd_core::{Action, Edge, Key, Step};

/// Linux key names read badly on a small cap; trim the common ones.
pub fn key_label(k: Key) -> String {
    let name = k.name().map_or_else(|| format!("#{}", k.0), str::to_owned);
    let short = name
        .strip_prefix("KEY_")
        .or_else(|| name.strip_prefix("BTN_"))
        .unwrap_or(&name);
    match short {
        "LEFTCTRL" | "RIGHTCTRL" => "Ctrl".into(),
        "LEFTSHIFT" | "RIGHTSHIFT" => "Shift".into(),
        "LEFTALT" | "RIGHTALT" => "Alt".into(),
        "LEFTMETA" | "RIGHTMETA" => "Super".into(),
        "SPACE" => "Space".into(),
        "ESC" => "Esc".into(),
        "PAGEUP" => "PgUp".into(),
        "PAGEDOWN" => "PgDn".into(),
        s if s.len() > 1 => {
            let mut c = s.chars();
            let first = c.next().expect("non-empty");
            format!("{first}{}", c.as_str().to_lowercase())
        }
        s => s.to_owned(),
    }
}

/// What a key cap shows for a binding.
pub fn describe(a: &Action) -> String {
    match a {
        Action::Passthrough => "pass".into(),
        Action::Disabled => "off".into(),
        Action::Chord(keys) => keys
            .iter()
            .map(|k| key_label(*k))
            .collect::<Vec<_>>()
            .join("+"),
        Action::Macro(steps) => format!("macro ({})", steps.len()),
        Action::SetMode(n) => format!("→ mode {}", n + 1),
        Action::NextMode => "next mode".into(),
    }
}

/// `KEY_LEFTCTRL+KEY_C`, the form profiles use.
pub fn chord_text(keys: &[Key]) -> String {
    keys.iter()
        .map(|k| k.name().map_or_else(|| format!("#{}", k.0), str::to_owned))
        .collect::<Vec<_>>()
        .join("+")
}

/// Render macro steps one per line. An adjacent press and release of the same key with
/// no delay between them is shown as a single tap line.
///
/// - `KEY_E` / `KEY_E 50`: tap, optionally after a delay (ms)
/// - `KEY_LEFTSHIFT down 0` / `KEY_LEFTSHIFT up 100`
pub fn macro_text(steps: &[Step]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < steps.len() {
        let s = &steps[i];
        let name = s
            .key
            .name()
            .map_or_else(|| format!("#{}", s.key.0), str::to_owned);
        let tap = s.edge == Edge::Down
            && steps
                .get(i + 1)
                .is_some_and(|n| n.key == s.key && n.edge == Edge::Up && n.delay_ms == 0);
        if tap {
            out.push_str(&format!("{name} {}\n", s.delay_ms));
            i += 2;
        } else {
            let edge = if s.edge == Edge::Down { "down" } else { "up" };
            out.push_str(&format!("{name} {edge} {}\n", s.delay_ms));
            i += 1;
        }
    }
    out
}

/// Parse the line format back into an [`Action::Macro`], reusing the profile format's own
/// validation (balance, delay range, length).
pub fn parse_macro(text: &str) -> Result<Action, String> {
    use toml::Value;
    let mut steps = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |why: &str| format!("line {}: {why}", n + 1);
        let parts: Vec<&str> = line.split_whitespace().collect();
        let num = |s: &str| {
            s.parse::<i64>()
                .map_err(|_| bad(&format!("`{s}` is not a number")))
        };
        let step = match parts.as_slice() {
            [key] => vec![Value::String((*key).into()), Value::Integer(0)],
            [key, ms] => vec![Value::String((*key).into()), Value::Integer(num(ms)?)],
            [key, edge @ ("down" | "up"), ms] => vec![
                Value::String((*key).into()),
                Value::String((*edge).into()),
                Value::Integer(num(ms)?),
            ],
            _ => return Err(bad("expected `KEY [ms]` or `KEY down|up ms`")),
        };
        steps.push(Value::Array(step));
    }
    let mut t = toml::Table::new();
    t.insert("macro".into(), Value::Array(steps));
    Action::from_toml(&Value::Table(t)).map_err(|e| e.to_string())
}

/// Parse chord text such as `KEY_LEFTCTRL+KEY_C`.
pub fn parse_chord(text: &str) -> Result<Action, String> {
    match Action::from_text(text) {
        Ok(a @ Action::Chord(_)) => Ok(a),
        Ok(_) => Err("that is an action name, not a key".into()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: &str) -> Key {
        Key::from_name(n).unwrap()
    }

    #[test]
    fn labels_are_short() {
        assert_eq!(key_label(key("KEY_R")), "R");
        assert_eq!(key_label(key("KEY_LEFTCTRL")), "Ctrl");
        assert_eq!(key_label(key("KEY_F1")), "F1");
        assert_eq!(key_label(key("KEY_TAB")), "Tab");
        assert_eq!(key_label(key("BTN_MIDDLE")), "Middle");
        assert_eq!(
            describe(&Action::Chord(vec![key("KEY_LEFTCTRL"), key("KEY_C")])),
            "Ctrl+C"
        );
        assert_eq!(describe(&Action::SetMode(1)), "→ mode 2");
    }

    #[test]
    fn chords_parse_and_reject_action_names() {
        let a = parse_chord("KEY_LEFTCTRL+KEY_C").unwrap();
        assert_eq!(a, Action::Chord(vec![key("KEY_LEFTCTRL"), key("KEY_C")]));
        assert!(parse_chord("disabled").is_err());
        assert!(parse_chord("KEY_NOPE").is_err());
        assert!(parse_chord("").is_err());
        assert_eq!(
            chord_text(&[key("KEY_LEFTCTRL"), key("KEY_C")]),
            "KEY_LEFTCTRL+KEY_C"
        );
    }

    #[test]
    fn macro_text_round_trips() {
        let src = "KEY_E 0\nKEY_F 50\nKEY_LEFTSHIFT down 0\nKEY_A 20\nKEY_LEFTSHIFT up 100\n";
        let Action::Macro(steps) = parse_macro(src).unwrap() else {
            panic!()
        };
        assert_eq!(macro_text(&steps), src);
        assert_eq!(
            parse_macro(&macro_text(&steps)).unwrap(),
            Action::Macro(steps)
        );
    }

    #[test]
    fn macro_errors_name_the_line() {
        assert!(parse_macro("").is_err(), "empty macro");
        let e = parse_macro("KEY_A 0\nKEY_B soon").unwrap_err();
        assert!(e.starts_with("line 2"), "{e}");
        assert!(
            parse_macro("KEY_A 0 0 0")
                .unwrap_err()
                .starts_with("line 1")
        );
        assert!(parse_macro("KEY_A down 0").is_err(), "unbalanced");
        assert!(parse_macro("KEY_NOPE").is_err());
        assert!(parse_macro("# comment\n\nKEY_A 5").is_ok());
    }
}
