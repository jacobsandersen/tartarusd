//! The Tartarus V2's physical layout as the editor draws it. Pure data.
//!
//! Each slot is identified by the [`PhysicalKey`] the device sends by default
//! (`docs/hardware.md`), because profiles bind physical keys by those codes.

use tartarusd_core::{Key, PhysicalKey, WheelDir};

#[derive(Clone, Copy, Debug)]
pub struct Slot {
    /// What the pad prints on the key: `01`..`20`, an arrow, or a wheel gesture.
    pub label: &'static str,
    /// The code the device sends for it.
    pub key: &'static str,
    /// Grid position and size, in cells.
    pub col: i32,
    pub row: i32,
    pub w: i32,
    pub h: i32,
}

const fn slot(label: &'static str, key: &'static str, col: i32, row: i32, w: i32) -> Slot {
    Slot {
        label,
        key,
        col,
        row,
        w,
        h: 1,
    }
}

/// Wheel directions are not `Key`s; they use this pseudo-name in [`Slot::key`].
pub const WHEEL_UP: &str = "WHEEL_UP";
pub const WHEEL_DOWN: &str = "WHEEL_DOWN";

pub const SLOTS: &[Slot] = &[
    slot("01", "KEY_1", 0, 0, 1),
    slot("02", "KEY_2", 1, 0, 1),
    slot("03", "KEY_3", 2, 0, 1),
    slot("04", "KEY_4", 3, 0, 1),
    slot("05", "KEY_5", 4, 0, 1),
    slot("06", "KEY_TAB", 0, 1, 1),
    slot("07", "KEY_Q", 1, 1, 1),
    slot("08", "KEY_W", 2, 1, 1),
    slot("09", "KEY_E", 3, 1, 1),
    slot("10", "KEY_R", 4, 1, 1),
    slot("11", "KEY_CAPSLOCK", 0, 2, 1),
    slot("12", "KEY_A", 1, 2, 1),
    slot("13", "KEY_S", 2, 2, 1),
    slot("14", "KEY_D", 3, 2, 1),
    slot("15", "KEY_F", 4, 2, 1),
    slot("16", "KEY_LEFTSHIFT", 0, 3, 1),
    slot("17", "KEY_Z", 1, 3, 1),
    slot("18", "KEY_X", 2, 3, 1),
    slot("19", "KEY_C", 3, 3, 1),
    // The D-pad.
    slot("▲", "KEY_UP", 6, 0, 1),
    slot("◀", "KEY_LEFT", 5, 1, 1),
    slot("▶", "KEY_RIGHT", 7, 1, 1),
    slot("▼", "KEY_DOWN", 6, 2, 1),
    // The large thumb button.
    slot("20", "KEY_SPACE", 5, 3, 3),
    // The scroll wheel.
    slot("Wheel ▲", WHEEL_UP, 8, 0, 1),
    slot("Click", "BTN_MIDDLE", 8, 1, 1),
    slot("Wheel ▼", WHEEL_DOWN, 8, 2, 1),
];

impl Slot {
    pub fn physical(&self) -> PhysicalKey {
        match self.key {
            WHEEL_UP => PhysicalKey::Wheel(WheelDir::Up),
            WHEEL_DOWN => PhysicalKey::Wheel(WheelDir::Down),
            name => PhysicalKey::Key(Key::from_name(name).expect("layout names are valid")),
        }
    }

    /// Whether this slot has a backlight LED that lighting frames can address.
    pub fn has_led(&self) -> bool {
        match self.physical() {
            PhysicalKey::Key(k) => tartarusd_core::lighting::cell(k).is_some(),
            PhysicalKey::Wheel(_) => false,
        }
    }
}

pub fn slot_for(key: PhysicalKey) -> Option<&'static Slot> {
    SLOTS.iter().find(|s| s.physical() == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_slot_resolves_and_is_unique() {
        let mut seen = HashSet::new();
        for s in SLOTS {
            assert!(seen.insert(s.physical()), "{} duplicated", s.label);
        }
    }

    #[test]
    fn covers_the_twenty_keys_dpad_and_wheel() {
        assert_eq!(SLOTS.len(), 20 + 4 + 3);
        let numbered = SLOTS
            .iter()
            .filter(|s| s.label.parse::<u8>().is_ok())
            .count();
        assert_eq!(numbered, 20);
    }

    #[test]
    fn slots_do_not_overlap() {
        let mut cells = HashSet::new();
        for s in SLOTS {
            for c in s.col..s.col + s.w {
                for r in s.row..s.row + s.h {
                    assert!(cells.insert((c, r)), "{} overlaps at {c},{r}", s.label);
                }
            }
        }
    }

    #[test]
    fn exactly_the_lit_keys_have_leds() {
        // 01..20 and the wheel click; the D-pad and wheel directions have no LED.
        let lit = SLOTS.iter().filter(|s| s.has_led()).count();
        assert_eq!(lit, 21);
        assert!(
            !slot_for(PhysicalKey::Key(Key::from_name("KEY_UP").unwrap()))
                .unwrap()
                .has_led()
        );
    }

    #[test]
    fn the_mode_key_is_not_a_slot() {
        assert!(slot_for(PhysicalKey::Key(Key::LEFTALT)).is_none());
    }
}
