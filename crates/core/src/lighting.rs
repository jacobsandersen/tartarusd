//! Per-key lighting: colors, the Tartarus V2 LED matrix map, and frame rendering.
//!
//! The matrix is 4 rows x 6 columns (docs/hardware.md, "Per-key matrix"). A frame is
//! rendered to exactly the bytes OpenRazer's `setKeyRow` expects: all four rows
//! concatenated, each as `row, 0, 5, R, G, B x 6` (84 bytes), written in one call.

use std::{collections::HashMap, fmt, str::FromStr};

use crate::keys::Key;

pub const ROWS: usize = 4;
pub const COLS: usize = 6;
pub const FRAME_LEN: usize = ROWS * (3 + COLS * 3);

/// A rendered frame, ready for `setKeyRow`.
pub type Payload = [u8; FRAME_LEN];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("`{0}` is not a color; use #rrggbb")]
pub struct BadColor(pub String);

impl FromStr for Rgb {
    type Err = BadColor;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || BadColor(s.to_owned());
        let hex = s.strip_prefix('#').ok_or_else(bad)?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        let n = u32::from_str_radix(hex, 16).map_err(|_| bad())?;
        Ok(Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// `(key, row, col)` for every key with an LED. The wheel's LED is addressed as
/// `BTN_MIDDLE` (the wheel click); the D-pad and the mode key have no LED.
const CELLS: &[(&str, usize, usize)] = &[
    ("KEY_1", 0, 0),
    ("KEY_2", 0, 1),
    ("KEY_3", 0, 2),
    ("KEY_4", 0, 3),
    ("KEY_5", 0, 4),
    ("KEY_TAB", 1, 0),
    ("KEY_Q", 1, 1),
    ("KEY_W", 1, 2),
    ("KEY_E", 1, 3),
    ("KEY_R", 1, 4),
    ("KEY_CAPSLOCK", 2, 0),
    ("KEY_A", 2, 1),
    ("KEY_S", 2, 2),
    ("KEY_D", 2, 3),
    ("KEY_F", 2, 4),
    ("KEY_LEFTSHIFT", 3, 0),
    ("KEY_Z", 3, 1),
    ("KEY_X", 3, 2),
    ("KEY_C", 3, 3),
    ("BTN_MIDDLE", 3, 4),
    ("KEY_SPACE", 3, 5),
];

/// The matrix cell for a key, if it has an LED.
pub fn cell(key: Key) -> Option<(usize, usize)> {
    CELLS
        .iter()
        .find(|(name, ..)| Key::from_name(name) == Some(key))
        .map(|&(_, r, c)| (r, c))
}

/// A static color per key, over a default color for everything else.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Frame {
    pub default: Rgb,
    pub keys: HashMap<Key, Rgb>,
}

impl Frame {
    /// Render to the 84-byte `setKeyRow` payload. Cells without a key (the unused
    /// column in rows 0-2) get the default color.
    pub fn render(&self) -> Payload {
        let mut grid = [[self.default; COLS]; ROWS];
        for (&key, &rgb) in &self.keys {
            if let Some((r, c)) = cell(key) {
                grid[r][c] = rgb;
            }
        }
        let mut out = [0u8; FRAME_LEN];
        for (r, row) in grid.iter().enumerate() {
            let base = r * (3 + COLS * 3);
            out[base..base + 3].copy_from_slice(&[r as u8, 0, (COLS - 1) as u8]);
            for (c, Rgb(red, green, blue)) in row.iter().enumerate() {
                let i = base + 3 + c * 3;
                out[i..i + 3].copy_from_slice(&[*red, *green, *blue]);
            }
        }
        out
    }
}

/// Which way a firmware wave travels, as seen looking at the pad.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Travel {
    /// Right to left (OpenRazer `setWave(1)`, confirmed on the device).
    Left,
    /// Left to right (OpenRazer `setWave(2)`).
    Right,
}

/// Highest speed value for [`PadEffect::Reactive`] (OpenRazer accepts 1..=4).
pub const REACTIVE_MAX_SPEED: u8 = 4;
/// Highest speed value for [`PadEffect::Starlight`].
pub const STARLIGHT_MAX_SPEED: u8 = 3;

/// A lighting effect run by the pad's firmware (through OpenRazer). It covers the whole
/// pad and cannot be combined with per-key colors.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PadEffect {
    Spectrum,
    Wave(Travel),
    /// 0 colors = random, 1 = one color, 2 = two colors.
    Breathing(Vec<Rgb>),
    Reactive {
        color: Rgb,
        speed: u8,
    },
    /// 0 colors = random, 1 = one color, 2 = two colors.
    Starlight {
        colors: Vec<Rgb>,
        speed: u8,
    },
}

/// What a layer shows: per-key colors, or one firmware effect.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Light {
    Frame(Frame),
    Effect(PadEffect),
}

/// What the pad should be doing right now.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Pad {
    /// Not ours: leave (or give back) the pad to OpenRazer's own effect.
    Handback,
    Frame(std::sync::Arc<Payload>),
    Effect(PadEffect),
}

impl Pad {
    pub fn frame(&self) -> Option<&std::sync::Arc<Payload>> {
        match self {
            Pad::Frame(f) => Some(f),
            _ => None,
        }
    }
}

fn effect_err(msg: impl Into<String>) -> String {
    msg.into()
}

fn colors_of(t: &toml::Table, key: &str, max: usize) -> Result<Vec<Rgb>, String> {
    let Some(v) = t.get(key) else {
        return Ok(Vec::new());
    };
    let arr = v
        .as_array()
        .ok_or_else(|| effect_err(format!("`{key}` must be a list of #rrggbb colors")))?;
    if arr.len() > max {
        return Err(effect_err(format!("`{key}` takes at most {max} colors")));
    }
    arr.iter()
        .map(|c| {
            c.as_str()
                .ok_or_else(|| effect_err(format!("`{key}` must be a list of #rrggbb colors")))?
                .parse::<Rgb>()
                .map_err(|e| effect_err(e.to_string()))
        })
        .collect()
}

fn speed_of(t: &toml::Table, max: u8) -> Result<u8, String> {
    match t.get("speed") {
        None => Ok(2),
        Some(v) => v
            .as_integer()
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| (1..=max).contains(n))
            .ok_or_else(|| effect_err(format!("`speed` must be 1..={max}"))),
    }
}

impl PadEffect {
    /// Parse an effect table: `type = "wave"`, `direction = "left"`, ... The error says
    /// what is wrong.
    pub fn from_table(t: &toml::Table) -> Result<PadEffect, String> {
        let kind = t
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| effect_err("an effect needs `type`"))?;
        let allowed: &[&str] = match kind {
            "spectrum" => &["type"],
            "wave" => &["type", "direction"],
            "breathing" => &["type", "colors"],
            "reactive" => &["type", "color", "speed"],
            "starlight" => &["type", "colors", "speed"],
            other => {
                return Err(effect_err(format!(
                    "unknown effect type `{other}` (spectrum, wave, breathing, reactive, starlight)"
                )));
            }
        };
        if let Some(extra) = t.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(effect_err(format!(
                "`{extra}` does not apply to a {kind} effect"
            )));
        }
        Ok(match kind {
            "spectrum" => PadEffect::Spectrum,
            "wave" => PadEffect::Wave(match t.get("direction").and_then(|v| v.as_str()) {
                None | Some("left") => Travel::Left,
                Some("right") => Travel::Right,
                Some(_) => return Err(effect_err("`direction` must be \"left\" or \"right\"")),
            }),
            "breathing" => PadEffect::Breathing(colors_of(t, "colors", 2)?),
            "reactive" => PadEffect::Reactive {
                color: t
                    .get("color")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| effect_err("a reactive effect needs `color`"))?
                    .parse()
                    .map_err(|e: BadColor| effect_err(e.to_string()))?,
                speed: speed_of(t, REACTIVE_MAX_SPEED)?,
            },
            _ => PadEffect::Starlight {
                colors: colors_of(t, "colors", 2)?,
                speed: speed_of(t, STARLIGHT_MAX_SPEED)?,
            },
        })
    }

    pub fn to_table(&self) -> toml::Table {
        use toml::Value;
        let mut t = toml::Table::new();
        let colors =
            |cs: &[Rgb]| Value::Array(cs.iter().map(|c| Value::String(c.to_string())).collect());
        let ty = |t: &mut toml::Table, n: &str| t.insert("type".into(), Value::String(n.into()));
        match self {
            PadEffect::Spectrum => {
                ty(&mut t, "spectrum");
            }
            PadEffect::Wave(d) => {
                ty(&mut t, "wave");
                let d = if *d == Travel::Left { "left" } else { "right" };
                t.insert("direction".into(), Value::String(d.into()));
            }
            PadEffect::Breathing(cs) => {
                ty(&mut t, "breathing");
                t.insert("colors".into(), colors(cs));
            }
            PadEffect::Reactive { color, speed } => {
                ty(&mut t, "reactive");
                t.insert("color".into(), Value::String(color.to_string()));
                t.insert("speed".into(), Value::Integer(i64::from(*speed)));
            }
            PadEffect::Starlight { colors: cs, speed } => {
                ty(&mut t, "starlight");
                t.insert("colors".into(), colors(cs));
                t.insert("speed".into(), Value::Integer(i64::from(*speed)));
            }
        }
        t
    }

    /// The effect as TOML text (used to preview it through the daemon).
    pub fn to_toml_string(&self) -> String {
        toml::to_string(&self.to_table()).expect("an effect always serializes")
    }

    pub fn from_toml_str(src: &str) -> Result<PadEffect, String> {
        let t: toml::Table = src.parse().map_err(|e: toml::de::Error| e.to_string())?;
        PadEffect::from_table(&t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: &str) -> Key {
        Key::from_name(n).unwrap()
    }

    #[test]
    fn effects_round_trip_through_toml() {
        let c = |n: &str| n.parse::<Rgb>().unwrap();
        for e in [
            PadEffect::Spectrum,
            PadEffect::Wave(Travel::Left),
            PadEffect::Wave(Travel::Right),
            PadEffect::Breathing(vec![]),
            PadEffect::Breathing(vec![c("#00ffff")]),
            PadEffect::Breathing(vec![c("#ff0000"), c("#0000ff")]),
            PadEffect::Reactive {
                color: c("#ff0000"),
                speed: 4,
            },
            PadEffect::Starlight {
                colors: vec![],
                speed: 1,
            },
            PadEffect::Starlight {
                colors: vec![c("#ff00ff"), c("#00ff00")],
                speed: 3,
            },
        ] {
            assert_eq!(PadEffect::from_toml_str(&e.to_toml_string()), Ok(e));
        }
    }

    #[test]
    fn effect_errors_say_what_is_wrong() {
        let e = |s: &str| PadEffect::from_toml_str(s).unwrap_err();
        assert!(e("").contains("needs `type`"));
        assert!(e("type = 'disco'").contains("unknown effect type"));
        assert!(e("type = 'spectrum'\nspeed = 2").contains("does not apply"));
        assert!(e("type = 'wave'\ndirection = 'up'").contains("left"));
        assert!(e("type = 'reactive'").contains("needs `color`"));
        assert!(e("type = 'reactive'\ncolor = '#ff0000'\nspeed = 5").contains("1..=4"));
        assert!(e("type = 'starlight'\nspeed = 4").contains("1..=3"));
        assert!(e("type = 'breathing'\ncolors = ['#fff']").contains("#rrggbb"));
        assert!(
            e("type = 'breathing'\ncolors = ['#ff0000','#00ff00','#0000ff']").contains("at most 2")
        );
        // Defaults: wave goes left, speed is 2.
        assert_eq!(
            PadEffect::from_toml_str("type = 'wave'"),
            Ok(PadEffect::Wave(Travel::Left))
        );
        assert_eq!(
            PadEffect::from_toml_str("type = 'starlight'"),
            Ok(PadEffect::Starlight {
                colors: vec![],
                speed: 2
            })
        );
    }

    #[test]
    fn colors() {
        assert_eq!("#00ffFF".parse(), Ok(Rgb(0, 255, 255)));
        assert_eq!(Rgb(1, 2, 255).to_string(), "#0102ff");
        for bad in ["00ffff", "#fff", "#gg0000", "#0000000", "", "#"] {
            assert!(bad.parse::<Rgb>().is_err(), "{bad}");
        }
    }

    #[test]
    fn cell_map_matches_hardware_doc() {
        // docs/hardware.md "Cell map (verified with tools/led_probe.py)".
        assert_eq!(cell(k("KEY_1")), Some((0, 0)));
        assert_eq!(cell(k("KEY_5")), Some((0, 4)));
        assert_eq!(cell(k("KEY_TAB")), Some((1, 0)));
        assert_eq!(cell(k("KEY_F")), Some((2, 4)));
        assert_eq!(cell(k("KEY_LEFTSHIFT")), Some((3, 0)));
        assert_eq!(cell(k("KEY_C")), Some((3, 3)));
        assert_eq!(cell(k("BTN_MIDDLE")), Some((3, 4)), "scroll wheel");
        assert_eq!(cell(k("KEY_SPACE")), Some((3, 5)), "thumb button");
        assert_eq!(cell(k("KEY_UP")), None, "D-pad has no LED");
        assert_eq!(cell(Key::LEFTALT), None, "mode key has no LED");
        let mut seen: Vec<_> = CELLS.iter().map(|&(_, r, c)| (r, c)).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 21, "21 distinct LEDs");
    }

    #[test]
    fn render_layout() {
        let mut keys = HashMap::new();
        keys.insert(k("KEY_W"), Rgb(1, 2, 3)); // (1, 2)
        keys.insert(k("KEY_SPACE"), Rgb(9, 8, 7)); // (3, 5)
        let p = Frame {
            default: Rgb(10, 20, 30),
            keys,
        }
        .render();
        assert_eq!(p.len(), 84);
        // Row headers: row, start col, end col.
        for r in 0..4 {
            assert_eq!(p[r * 21..r * 21 + 3], [r as u8, 0, 5]);
        }
        let at = |r: usize, c: usize| p[r * 21 + 3 + c * 3..r * 21 + 6 + c * 3].to_vec();
        assert_eq!(at(1, 2), [1, 2, 3]);
        assert_eq!(at(3, 5), [9, 8, 7]);
        assert_eq!(at(0, 0), [10, 20, 30], "default fills the rest");
        assert_eq!(at(0, 5), [10, 20, 30], "unused cell gets the default");
    }
}
