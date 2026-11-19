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

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: &str) -> Key {
        Key::from_name(n).unwrap()
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
