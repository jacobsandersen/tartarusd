//! Linux key codes and the physical inputs of the Tartarus.

use std::{collections::HashMap, fmt, str::FromStr, sync::OnceLock};

use crate::keys_table::KEY_TABLE;

/// A Linux `EV_KEY` code (keyboard keys and `BTN_*` buttons).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Key(pub u16);

type Tables = (HashMap<&'static str, u16>, HashMap<u16, &'static str>);

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut by_name = HashMap::new();
        let mut by_code = HashMap::new();
        for &(name, code) in KEY_TABLE {
            by_name.insert(name, code);
            // First name wins for a code, so aliases never shadow the canonical name.
            by_code.entry(code).or_insert(name);
        }
        (by_name, by_code)
    })
}

impl Key {
    /// The Tartarus hyper/mode key (`KEY_LEFTALT`) by default.
    pub const LEFTALT: Key = Key(56);

    /// Look up a `KEY_*` / `BTN_*` name.
    pub fn from_name(name: &str) -> Option<Key> {
        tables().0.get(name).copied().map(Key)
    }

    /// Every known key with its canonical name (`KEY_*` and `BTN_*`).
    pub fn all_named() -> impl Iterator<Item = (Key, &'static str)> {
        KEY_TABLE.iter().map(|&(name, code)| (Key(code), name))
    }

    /// Canonical name, if the code is known.
    pub fn name(self) -> Option<&'static str> {
        tables().1.get(&self.0).copied()
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(n) => f.write_str(n),
            None => write!(f, "KEY_#{}", self.0),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown key name `{0}`")]
pub struct UnknownKey(pub String);

impl FromStr for Key {
    type Err = UnknownKey;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Key::from_name(s).ok_or_else(|| UnknownKey(s.to_owned()))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum WheelDir {
    Up,
    Down,
}

/// Anything on the device that can be bound: a key/button code, or a wheel direction.
/// (The wheel click is `BTN_MIDDLE`, an ordinary [`Key`].)
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum PhysicalKey {
    Key(Key),
    Wheel(WheelDir),
}

impl fmt::Display for PhysicalKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PhysicalKey::Key(k) => k.fmt(f),
            PhysicalKey::Wheel(WheelDir::Up) => f.write_str("WHEEL_UP"),
            PhysicalKey::Wheel(WheelDir::Down) => f.write_str("WHEEL_DOWN"),
        }
    }
}

impl FromStr for PhysicalKey {
    type Err = UnknownKey;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "WHEEL_UP" => Ok(PhysicalKey::Wheel(WheelDir::Up)),
            "WHEEL_DOWN" => Ok(PhysicalKey::Wheel(WheelDir::Down)),
            _ => s.parse().map(PhysicalKey::Key),
        }
    }
}

impl From<Key> for PhysicalKey {
    fn from(k: Key) -> Self {
        PhysicalKey::Key(k)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Edge {
    Down,
    Up,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        assert_eq!(Key::from_name("KEY_LEFTALT"), Some(Key::LEFTALT));
        assert_eq!(Key::LEFTALT.name(), Some("KEY_LEFTALT"));
        assert_eq!(Key::from_name("BTN_MIDDLE").map(|k| k.0), Some(0x112));
        assert_eq!(Key::from_name("KEY_NOPE"), None);
    }

    #[test]
    fn verified_tartarus_codes() {
        // docs/hardware.md default codes (evdev numbers from the input-remapper sample).
        for (name, code) in [
            ("KEY_1", 2),
            ("KEY_TAB", 15),
            ("KEY_Q", 16),
            ("KEY_CAPSLOCK", 58),
            ("KEY_A", 30),
            ("KEY_LEFTSHIFT", 42),
            ("KEY_Z", 44),
            ("KEY_SPACE", 57),
            ("KEY_UP", 103),
            ("KEY_LEFT", 105),
            ("KEY_RIGHT", 106),
            ("KEY_DOWN", 108),
        ] {
            assert_eq!(Key::from_name(name), Some(Key(code)), "{name}");
        }
    }

    #[test]
    fn physical_key_parse() {
        assert_eq!("WHEEL_UP".parse(), Ok(PhysicalKey::Wheel(WheelDir::Up)));
        assert_eq!("KEY_Q".parse(), Ok(PhysicalKey::Key(Key(16))));
        assert!("bogus".parse::<PhysicalKey>().is_err());
        assert_eq!(PhysicalKey::Wheel(WheelDir::Down).to_string(), "WHEEL_DOWN");
    }
}
