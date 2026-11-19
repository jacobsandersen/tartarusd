//! What a bound key does, and its TOML syntax.
//!
//! ```toml
//! KEY_1 = "KEY_R"                        # single-key chord
//! KEY_2 = "KEY_LEFTCTRL+KEY_C"           # chord
//! KEY_3 = "disabled"                     # swallow
//! KEY_4 = "passthrough"                  # explicit default
//! KEY_5 = "next_mode"
//! KEY_6 = { set_mode = 1 }
//! KEY_Q = { macro = [["KEY_E", 0], ["KEY_F", 50]] }   # tap E, then 50 ms later tap F
//! KEY_W = { macro = [["KEY_LEFTSHIFT", "down", 0], ["KEY_A", 10], ["KEY_LEFTSHIFT", "up", 10]] }
//! ```
//!
//! Macro steps are `[key, delay_ms]` (a tap: down then up, delay before the down) or
//! `[key, "down"|"up", delay_ms]`. A macro must be balanced: every key it presses is
//! released by the end, and it never releases a key it did not press.

use std::sync::Arc;

use crate::keys::{Edge, Key, UnknownKey};

/// Upper bound for a single macro step delay.
pub const MAX_STEP_DELAY_MS: i64 = 60_000;
/// Upper bound for the number of steps in one macro.
pub const MAX_MACRO_STEPS: usize = 512;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Step {
    pub key: Key,
    pub edge: Edge,
    /// Delay before this step is performed.
    pub delay_ms: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Forward the physical input unchanged.
    Passthrough,
    /// Swallow the input.
    Disabled,
    /// Press all keys in order on key-down, release in reverse on key-up.
    Chord(Vec<Key>),
    /// Timed sequence; executed by the daemon, not the state machine.
    Macro(Arc<[Step]>),
    SetMode(u8),
    NextMode,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ActionError {
    #[error(transparent)]
    UnknownKey(#[from] UnknownKey),
    #[error("empty chord")]
    EmptyChord,
    #[error("expected a string or a table")]
    WrongType,
    #[error("action table must have exactly one of `macro` or `set_mode`")]
    BadTable,
    #[error("`set_mode` expects an integer 0..=6")]
    BadSetMode,
    #[error("invalid macro: {0}")]
    BadMacro(String),
}

impl Action {
    pub fn from_toml(v: &toml::Value) -> Result<Action, ActionError> {
        match v {
            toml::Value::String(s) => Self::from_text(s),
            toml::Value::Table(t) => {
                if t.len() != 1 {
                    return Err(ActionError::BadTable);
                }
                let (k, v) = t.iter().next().expect("len checked");
                match k.as_str() {
                    "macro" => parse_macro(v),
                    "set_mode" => v
                        .as_integer()
                        .and_then(|n| u8::try_from(n).ok())
                        .filter(|n| *n < 7)
                        .map(Action::SetMode)
                        .ok_or(ActionError::BadSetMode),
                    _ => Err(ActionError::BadTable),
                }
            }
            _ => Err(ActionError::WrongType),
        }
    }

    /// Parse the string forms: `passthrough`, `disabled`, `next_mode`, or a `+` chord.
    pub fn from_text(s: &str) -> Result<Action, ActionError> {
        match s.trim() {
            "passthrough" => Ok(Action::Passthrough),
            "disabled" => Ok(Action::Disabled),
            "next_mode" => Ok(Action::NextMode),
            chord => {
                let keys = chord
                    .split('+')
                    .map(|p| {
                        let p = p.trim();
                        if p.is_empty() {
                            Err(ActionError::EmptyChord)
                        } else {
                            Ok(p.parse::<Key>()?)
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Action::Chord(keys))
            }
        }
    }
}

fn delay(v: &toml::Value) -> Result<u32, ActionError> {
    v.as_integer()
        .filter(|n| (0..=MAX_STEP_DELAY_MS).contains(n))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(ActionError::BadMacro(
            "delay must be an integer 0..=60000".into(),
        ))
}

fn parse_macro(v: &toml::Value) -> Result<Action, ActionError> {
    let steps = v.as_array().ok_or(ActionError::BadMacro(
        "`macro` must be an array of steps".into(),
    ))?;
    if steps.is_empty() {
        return Err(ActionError::BadMacro("macro has no steps".into()));
    }
    let mut out = Vec::new();
    for step in steps {
        let parts = step
            .as_array()
            .ok_or(ActionError::BadMacro("each step must be an array".into()))?;
        let key = parts
            .first()
            .and_then(|k| k.as_str())
            .ok_or(ActionError::BadMacro(
                "step must start with a key name".into(),
            ))?
            .parse::<Key>()?;
        match parts.as_slice() {
            [_, d] => {
                out.push(Step {
                    key,
                    edge: Edge::Down,
                    delay_ms: delay(d)?,
                });
                out.push(Step {
                    key,
                    edge: Edge::Up,
                    delay_ms: 0,
                });
            }
            [_, e, d] => {
                let edge = match e.as_str() {
                    Some("down") => Edge::Down,
                    Some("up") => Edge::Up,
                    _ => {
                        return Err(ActionError::BadMacro(
                            "edge must be \"down\" or \"up\"".into(),
                        ));
                    }
                };
                out.push(Step {
                    key,
                    edge,
                    delay_ms: delay(d)?,
                });
            }
            _ => {
                return Err(ActionError::BadMacro(
                    "step must have 2 or 3 elements".into(),
                ));
            }
        }
    }
    if out.len() > MAX_MACRO_STEPS {
        return Err(ActionError::BadMacro(format!(
            "{} steps; the maximum is {MAX_MACRO_STEPS}",
            out.len()
        )));
    }
    check_balanced(&out)?;
    Ok(Action::Macro(out.into()))
}

/// A macro must leave every key as it found it: no key pressed twice, none released
/// without being pressed (that would release a physical key's output), none left down.
fn check_balanced(steps: &[Step]) -> Result<(), ActionError> {
    let mut down = std::collections::HashSet::new();
    for s in steps {
        match s.edge {
            Edge::Down if !down.insert(s.key) => {
                return Err(ActionError::BadMacro(format!(
                    "{} is pressed twice without being released",
                    s.key
                )));
            }
            Edge::Up if !down.remove(&s.key) => {
                return Err(ActionError::BadMacro(format!(
                    "{} is released without being pressed",
                    s.key
                )));
            }
            _ => {}
        }
    }
    match down.iter().min() {
        Some(k) => Err(ActionError::BadMacro(format!("{k} is never released"))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Action, ActionError> {
        let t: toml::Table = toml::from_str(&format!("a = {s}")).unwrap();
        Action::from_toml(&t["a"])
    }

    #[test]
    fn simple_forms() {
        assert_eq!(parse(r#""passthrough""#), Ok(Action::Passthrough));
        assert_eq!(parse(r#""disabled""#), Ok(Action::Disabled));
        assert_eq!(parse(r#""next_mode""#), Ok(Action::NextMode));
        assert_eq!(parse("{ set_mode = 2 }"), Ok(Action::SetMode(2)));
        assert_eq!(
            parse(r#""KEY_R""#),
            Ok(Action::Chord(vec![Key::from_name("KEY_R").unwrap()]))
        );
    }

    #[test]
    fn chord() {
        let Ok(Action::Chord(k)) = parse(r#""KEY_LEFTCTRL + KEY_C""#) else {
            panic!()
        };
        assert_eq!(k.len(), 2);
    }

    #[test]
    fn errors() {
        assert!(matches!(
            parse(r#""KEY_NOPE""#),
            Err(ActionError::UnknownKey(_))
        ));
        assert_eq!(parse(r#""KEY_A+""#), Err(ActionError::EmptyChord));
        assert_eq!(parse("{ set_mode = 7 }"), Err(ActionError::BadSetMode));
        assert_eq!(
            parse("{ set_mode = 1, macro = [] }"),
            Err(ActionError::BadTable)
        );
        assert_eq!(parse("5"), Err(ActionError::WrongType));
        assert!(matches!(
            parse("{ macro = [] }"),
            Err(ActionError::BadMacro(_))
        ));
        assert!(matches!(
            parse(r#"{ macro = [["KEY_A", -1]] }"#),
            Err(ActionError::BadMacro(_))
        ));
    }

    #[test]
    fn macros_must_be_balanced() {
        let bad = |m: &str| match parse(&format!("{{ macro = {m} }}")) {
            Err(ActionError::BadMacro(e)) => e,
            other => panic!("expected BadMacro, got {other:?}"),
        };
        assert!(bad(r#"[["KEY_A", "down", 0]]"#).contains("never released"));
        assert!(bad(r#"[["KEY_A", "up", 0]]"#).contains("without being pressed"));
        assert!(bad(r#"[["KEY_A", "down", 0], ["KEY_A", "down", 0]]"#).contains("twice"));
        // Balanced, including interleaved holds, is fine.
        assert!(parse(
            r#"{ macro = [["KEY_LEFTSHIFT", "down", 0], ["KEY_A", 10], ["KEY_LEFTSHIFT", "up", 0]] }"#
        )
        .is_ok());
    }

    #[test]
    fn macro_length_is_capped() {
        let steps = vec![r#"["KEY_A", 0]"#; MAX_MACRO_STEPS / 2 + 1].join(",");
        assert!(matches!(
            parse(&format!("{{ macro = [{steps}] }}")),
            Err(ActionError::BadMacro(e)) if e.contains("maximum")
        ));
    }

    #[test]
    fn macro_steps() {
        let Ok(Action::Macro(s)) =
            parse(r#"{ macro = [["KEY_E", 0], ["KEY_F", "down", 50], ["KEY_F", "up", 5]] }"#)
        else {
            panic!()
        };
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].edge, Edge::Down);
        assert_eq!(s[1].edge, Edge::Up);
        assert_eq!(s[2].delay_ms, 50);
        assert_eq!(s[3].delay_ms, 5);
    }
}
