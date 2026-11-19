//! Editing operations on a [`Profile`], kept free of GTK so they can be tested.
//!
//! Every function leaves the profile in a state [`crate::store::validate`] accepts, or
//! returns an error without changing anything.

use tartarusd_core::{
    Action, Frame, Key, PhysicalKey, Profile, Rgb,
    profile::{DEFAULT_LED_MASKS, MAX_MODES_DEFAULT, MAX_MODES_EXTENDED, Mode},
};

/// Which of a mode's two layers an edit applies to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    Base,
    Hypershift,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EditError {
    #[error("a profile has at most {0} modes (turn on extended modes for up to 7)")]
    TooManyModes(usize),
    #[error("a profile needs at least one mode")]
    LastMode,
    #[error("another mode already uses that LED combination")]
    LedInUse,
    #[error("LED combination must be 1..=7")]
    BadLed,
    #[error("remove modes until at most 3 are left before turning extended modes off")]
    TooManyForDefault,
}

pub fn binding(p: &Profile, mode: usize, layer: Layer, key: PhysicalKey) -> Option<&Action> {
    let m = p.modes.get(mode)?;
    match layer {
        Layer::Base => m.bindings.get(&key),
        Layer::Hypershift => m.hypershift.as_ref()?.get(&key),
    }
}

/// Set or clear (`None`) one binding. An emptied Hypershift layer becomes absent.
pub fn set_binding(
    p: &mut Profile,
    mode: usize,
    layer: Layer,
    key: PhysicalKey,
    a: Option<Action>,
) {
    let Some(m) = p.modes.get_mut(mode) else {
        return;
    };
    match layer {
        Layer::Base => {
            match a {
                Some(a) => m.bindings.insert(key, a),
                None => m.bindings.remove(&key),
            };
        }
        Layer::Hypershift => match a {
            Some(a) => {
                m.hypershift
                    .get_or_insert_with(Default::default)
                    .insert(key, a);
            }
            None => {
                if let Some(h) = m.hypershift.as_mut() {
                    h.remove(&key);
                    if h.is_empty() {
                        m.hypershift = None;
                    }
                }
            }
        },
    }
}

pub fn frame(p: &Profile, mode: usize, layer: Layer) -> Option<&Frame> {
    let m = p.modes.get(mode)?;
    match layer {
        Layer::Base => m.lighting.as_ref(),
        Layer::Hypershift => m.hypershift_lighting.as_ref(),
    }
}

fn frame_slot(m: &mut Mode, layer: Layer) -> &mut Option<Frame> {
    match layer {
        Layer::Base => &mut m.lighting,
        Layer::Hypershift => &mut m.hypershift_lighting,
    }
}

/// Give a layer its own lighting frame (all dark) or remove it.
pub fn set_frame_enabled(p: &mut Profile, mode: usize, layer: Layer, on: bool) {
    let Some(m) = p.modes.get_mut(mode) else {
        return;
    };
    let slot = frame_slot(m, layer);
    match (on, slot.is_some()) {
        (true, false) => *slot = Some(Frame::default()),
        (false, true) => *slot = None,
        _ => {}
    }
}

pub fn set_default_color(p: &mut Profile, mode: usize, layer: Layer, c: Rgb) {
    if let Some(f) = p
        .modes
        .get_mut(mode)
        .and_then(|m| frame_slot(m, layer).as_mut())
    {
        f.default = c;
    }
}

/// Set one key's color, or clear it (`None`) so it shows the frame's default.
pub fn set_color(p: &mut Profile, mode: usize, layer: Layer, key: Key, c: Option<Rgb>) {
    if let Some(f) = p
        .modes
        .get_mut(mode)
        .and_then(|m| frame_slot(m, layer).as_mut())
    {
        match c {
            Some(c) => f.keys.insert(key, c),
            None => f.keys.remove(&key),
        };
    }
}

pub fn max_modes(p: &Profile) -> usize {
    if p.extended_modes {
        MAX_MODES_EXTENDED
    } else {
        MAX_MODES_DEFAULT
    }
}

/// Append an empty mode with the first LED combination no other mode uses.
pub fn add_mode(p: &mut Profile) -> Result<usize, EditError> {
    if p.modes.len() >= max_modes(p) {
        return Err(EditError::TooManyModes(max_modes(p)));
    }
    let mask = DEFAULT_LED_MASKS
        .iter()
        .copied()
        .find(|m| p.modes.iter().all(|x| x.led_mask != *m))
        .expect("7 masks cover at most 7 modes");
    p.modes.push(Mode {
        led_mask: mask,
        bindings: Default::default(),
        hypershift: None,
        lighting: None,
        hypershift_lighting: None,
    });
    Ok(p.modes.len() - 1)
}

/// Remove a mode. Bindings that switched to it are cleared, and ones that switched to a
/// later mode follow it down one index.
pub fn remove_mode(p: &mut Profile, idx: usize) -> Result<(), EditError> {
    if p.modes.len() <= 1 {
        return Err(EditError::LastMode);
    }
    if idx >= p.modes.len() {
        return Ok(());
    }
    p.modes.remove(idx);
    let target = idx as u8;
    let fix = |layer: &mut std::collections::HashMap<PhysicalKey, Action>| {
        layer.retain(|_, a| !matches!(a, Action::SetMode(t) if *t == target));
        for a in layer.values_mut() {
            if let Action::SetMode(t) = a
                && *t > target
            {
                *t -= 1;
            }
        }
    };
    for m in &mut p.modes {
        fix(&mut m.bindings);
        if let Some(h) = m.hypershift.as_mut() {
            fix(h);
        }
        if m.hypershift.as_ref().is_some_and(|h| h.is_empty()) {
            m.hypershift = None;
        }
    }
    Ok(())
}

pub fn set_led_mask(p: &mut Profile, mode: usize, mask: u8) -> Result<(), EditError> {
    if !(1..=7).contains(&mask) {
        return Err(EditError::BadLed);
    }
    if p.modes
        .iter()
        .enumerate()
        .any(|(i, m)| i != mode && m.led_mask == mask)
    {
        return Err(EditError::LedInUse);
    }
    if let Some(m) = p.modes.get_mut(mode) {
        m.led_mask = mask;
    }
    Ok(())
}

pub fn set_extended(p: &mut Profile, on: bool) -> Result<(), EditError> {
    if !on && p.modes.len() > MAX_MODES_DEFAULT {
        return Err(EditError::TooManyForDefault);
    }
    p.extended_modes = on;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{blank, validate};

    fn k(name: &str) -> PhysicalKey {
        PhysicalKey::Key(Key::from_name(name).unwrap())
    }

    #[test]
    fn bindings_set_clear_and_hypershift_layer_appears_and_disappears() {
        let mut p = blank("x");
        let one = k("KEY_1");
        set_binding(&mut p, 0, Layer::Base, one, Some(Action::Disabled));
        assert_eq!(binding(&p, 0, Layer::Base, one), Some(&Action::Disabled));
        set_binding(&mut p, 0, Layer::Hypershift, one, Some(Action::NextMode));
        assert!(p.modes[0].hypershift.is_some());
        set_binding(&mut p, 0, Layer::Hypershift, one, None);
        assert!(p.modes[0].hypershift.is_none(), "an empty layer is absent");
        set_binding(&mut p, 0, Layer::Base, one, None);
        assert_eq!(binding(&p, 0, Layer::Base, one), None);
        validate(&p).unwrap();
    }

    #[test]
    fn modes_are_capped_and_get_distinct_leds() {
        let mut p = blank("x");
        assert_eq!(add_mode(&mut p), Ok(1));
        assert_eq!(add_mode(&mut p), Ok(2));
        assert_eq!(add_mode(&mut p), Err(EditError::TooManyModes(3)));
        let masks: Vec<u8> = p.modes.iter().map(|m| m.led_mask).collect();
        assert_eq!(masks, [1, 2, 4]);
        set_extended(&mut p, true).unwrap();
        for _ in 3..7 {
            add_mode(&mut p).unwrap();
        }
        assert_eq!(add_mode(&mut p), Err(EditError::TooManyModes(7)));
        validate(&p).unwrap();
        assert_eq!(
            set_extended(&mut p, false),
            Err(EditError::TooManyForDefault)
        );
    }

    #[test]
    fn a_freed_led_combination_is_reused() {
        let mut p = blank("x");
        add_mode(&mut p).unwrap();
        add_mode(&mut p).unwrap();
        remove_mode(&mut p, 1).unwrap();
        assert_eq!(add_mode(&mut p), Ok(2));
        assert_eq!(p.modes[2].led_mask, 2);
        validate(&p).unwrap();
    }

    #[test]
    fn removing_a_mode_repairs_set_mode_references() {
        let mut p = blank("x");
        for _ in 0..2 {
            add_mode(&mut p).unwrap();
        }
        let (a, b, c) = (k("KEY_1"), k("KEY_2"), k("KEY_3"));
        set_binding(&mut p, 0, Layer::Base, a, Some(Action::SetMode(1)));
        set_binding(&mut p, 0, Layer::Base, b, Some(Action::SetMode(2)));
        set_binding(&mut p, 2, Layer::Hypershift, c, Some(Action::SetMode(1)));
        remove_mode(&mut p, 1).unwrap();
        assert_eq!(p.modes.len(), 2);
        assert_eq!(
            binding(&p, 0, Layer::Base, a),
            None,
            "pointed at the removed mode"
        );
        assert_eq!(binding(&p, 0, Layer::Base, b), Some(&Action::SetMode(1)));
        assert!(
            p.modes[1].hypershift.is_none(),
            "its only entry was cleared"
        );
        validate(&p).unwrap();
    }

    #[test]
    fn the_last_mode_cannot_be_removed() {
        let mut p = blank("x");
        assert_eq!(remove_mode(&mut p, 0), Err(EditError::LastMode));
        assert_eq!(p.modes.len(), 1);
    }

    #[test]
    fn led_masks_must_be_valid_and_unique() {
        let mut p = blank("x");
        add_mode(&mut p).unwrap();
        assert_eq!(set_led_mask(&mut p, 1, 1), Err(EditError::LedInUse));
        assert_eq!(set_led_mask(&mut p, 1, 0), Err(EditError::BadLed));
        assert_eq!(set_led_mask(&mut p, 1, 8), Err(EditError::BadLed));
        set_led_mask(&mut p, 1, 7).unwrap();
        set_led_mask(&mut p, 0, 1).unwrap(); // its own value is fine
        validate(&p).unwrap();
    }

    #[test]
    fn lighting_frames_are_opt_in_per_layer() {
        let mut p = blank("x");
        let w = Key::from_name("KEY_W").unwrap();
        set_color(&mut p, 0, Layer::Base, w, Some(Rgb(1, 2, 3)));
        assert!(
            frame(&p, 0, Layer::Base).is_none(),
            "no frame, nothing to color"
        );
        set_frame_enabled(&mut p, 0, Layer::Base, true);
        set_default_color(&mut p, 0, Layer::Base, Rgb(9, 9, 9));
        set_color(&mut p, 0, Layer::Base, w, Some(Rgb(1, 2, 3)));
        assert!(p.uses_lighting());
        validate(&p).unwrap();
        let f = frame(&p, 0, Layer::Base).unwrap();
        assert_eq!(f.keys[&w], Rgb(1, 2, 3));
        assert_eq!(f.default, Rgb(9, 9, 9));
        set_color(&mut p, 0, Layer::Base, w, None);
        assert!(frame(&p, 0, Layer::Base).unwrap().keys.is_empty());
        set_frame_enabled(&mut p, 0, Layer::Base, false);
        assert!(!p.uses_lighting());
    }
}
