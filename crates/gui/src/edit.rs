//! Editing operations on a [`Profile`], kept free of GTK so they can be tested.
//!
//! Every function leaves the profile in a state [`crate::store::validate`] accepts, or
//! returns an error without changing anything.

use tartarusd_core::{
    Action, Frame, Key, Light, PadEffect, PhysicalKey, Profile, Rgb, Travel,
    lighting::{REACTIVE_MAX_SPEED, STARLIGHT_MAX_SPEED},
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
    #[error("At most {0} modes; turn on extended modes for 7")]
    TooManyModes(usize),
    #[error("A profile needs at least one mode")]
    LastMode,
    #[error("Another mode uses these LEDs")]
    LedInUse,
    #[error("Select at least one LED")]
    BadLed,
    #[error("Remove modes to have 3 or fewer first")]
    TooManyForDefault,
    #[error("The base layer has no lighting to copy")]
    NothingToCopy,
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

/// Set or clear the same binding on several keys.
pub fn set_bindings(
    p: &mut Profile,
    mode: usize,
    layer: Layer,
    keys: &[PhysicalKey],
    a: Option<Action>,
) {
    for k in keys {
        set_binding(p, mode, layer, *k, a.clone());
    }
}

/// What `keys` all have in common: `Some(binding)` (possibly unbound) if every key has the
/// same one, `None` if they differ.
pub fn common_binding(
    p: &Profile,
    mode: usize,
    layer: Layer,
    keys: &[PhysicalKey],
) -> Option<Option<Action>> {
    let mut it = keys.iter().map(|k| binding(p, mode, layer, *k).cloned());
    let first = it.next()?;
    it.all(|b| b == first).then_some(first)
}

fn light(p: &Profile, mode: usize, layer: Layer) -> Option<&Light> {
    let m = p.modes.get(mode)?;
    match layer {
        Layer::Base => m.lighting.as_ref(),
        Layer::Hypershift => m.hypershift_lighting.as_ref(),
    }
}

/// The layer's per-key frame, if its lighting is key colors.
pub fn frame(p: &Profile, mode: usize, layer: Layer) -> Option<&Frame> {
    match light(p, mode, layer)? {
        Light::Frame(f) => Some(f),
        Light::Effect(_) => None,
    }
}

/// The layer's firmware effect, if its lighting is an effect.
pub fn effect(p: &Profile, mode: usize, layer: Layer) -> Option<&PadEffect> {
    match light(p, mode, layer)? {
        Light::Effect(e) => Some(e),
        Light::Frame(_) => None,
    }
}

fn light_slot(m: &mut Mode, layer: Layer) -> &mut Option<Light> {
    match layer {
        Layer::Base => &mut m.lighting,
        Layer::Hypershift => &mut m.hypershift_lighting,
    }
}

fn frame_mut(m: &mut Mode, layer: Layer) -> Option<&mut Frame> {
    match light_slot(m, layer) {
        Some(Light::Frame(f)) => Some(f),
        _ => None,
    }
}

/// What a layer's lighting is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LightKind {
    /// The layer sets nothing (Hypershift then keeps the mode's lighting).
    Off,
    Keys,
    Effect,
}

pub fn kind(p: &Profile, mode: usize, layer: Layer) -> LightKind {
    match light(p, mode, layer) {
        None => LightKind::Off,
        Some(Light::Frame(_)) => LightKind::Keys,
        Some(Light::Effect(_)) => LightKind::Effect,
    }
}

/// Change what a layer's lighting is. Choosing the kind it already has keeps its contents.
pub fn set_kind(p: &mut Profile, mode: usize, layer: Layer, to: LightKind) {
    if kind(p, mode, layer) == to {
        return;
    }
    let Some(m) = p.modes.get_mut(mode) else {
        return;
    };
    *light_slot(m, layer) = match to {
        LightKind::Off => None,
        LightKind::Keys => Some(Light::Frame(Frame::default())),
        LightKind::Effect => Some(Light::Effect(PadEffect::Spectrum)),
    };
}

/// Change the layer's effect in place (no-op unless its lighting is an effect).
pub fn update_effect(p: &mut Profile, mode: usize, layer: Layer, f: impl FnOnce(&mut PadEffect)) {
    if let Some(Light::Effect(e)) = p
        .modes
        .get_mut(mode)
        .and_then(|m| light_slot(m, layer).as_mut())
    {
        f(e);
    }
}

/// `c` scaled to `pct` percent brightness (above 100 brightens, clamped at full).
fn scale(c: Rgb, pct: u32) -> Rgb {
    let ch = |v: u8| ((u32::from(v) * pct + 50) / 100).min(255) as u8;
    Rgb(ch(c.0), ch(c.1), ch(c.2))
}

/// A copy of `light` with every color scaled to `pct` percent. Effects without colors
/// (spectrum, wave, random breathing or starlight) are unchanged.
pub fn scale_light(light: &Light, pct: u32) -> Light {
    match light {
        Light::Frame(f) => Light::Frame(Frame {
            default: scale(f.default, pct),
            keys: f.keys.iter().map(|(k, c)| (*k, scale(*c, pct))).collect(),
        }),
        Light::Effect(e) => Light::Effect(match e {
            PadEffect::Breathing(c) => {
                PadEffect::Breathing(c.iter().map(|c| scale(*c, pct)).collect())
            }
            PadEffect::Reactive { color, speed } => PadEffect::Reactive {
                color: scale(*color, pct),
                speed: *speed,
            },
            PadEffect::Starlight { colors, speed } => PadEffect::Starlight {
                colors: colors.iter().map(|c| scale(*c, pct)).collect(),
                speed: *speed,
            },
            other => other.clone(),
        }),
    }
}

/// Scale the colors of one layer's lighting in place.
pub fn scale_layer(p: &mut Profile, mode: usize, layer: Layer, pct: u32) {
    if let Some(l) = p
        .modes
        .get_mut(mode)
        .and_then(|m| light_slot(m, layer).as_mut())
    {
        *l = scale_light(l, pct);
    }
}

/// Make `to` a copy of `from`'s lighting, scaled to `pct` percent. `false` (nothing
/// changed) if `from` has no lighting.
pub fn copy_layer(p: &mut Profile, mode: usize, from: Layer, to: Layer, pct: u32) -> bool {
    let Some(copy) = light(p, mode, from).map(|l| scale_light(l, pct)) else {
        return false;
    };
    if let Some(m) = p.modes.get_mut(mode) {
        *light_slot(m, to) = Some(copy);
    }
    true
}

/// The effect types the editor offers, in menu order.
pub const EFFECT_TYPES: [&str; 5] = ["Spectrum", "Wave", "Breathing", "Reactive", "Starlight"];

pub fn effect_type_index(e: &PadEffect) -> u32 {
    match e {
        PadEffect::Spectrum => 0,
        PadEffect::Wave(_) => 1,
        PadEffect::Breathing(_) => 2,
        PadEffect::Reactive { .. } => 3,
        PadEffect::Starlight { .. } => 4,
    }
}

/// Switch to another effect type, carrying over colors and speed where they make sense.
pub fn with_effect_type(old: &PadEffect, index: u32) -> PadEffect {
    let colors: Vec<Rgb> = match old {
        PadEffect::Breathing(c) | PadEffect::Starlight { colors: c, .. } => c.clone(),
        PadEffect::Reactive { color, .. } => vec![*color],
        _ => Vec::new(),
    };
    let speed = match old {
        PadEffect::Reactive { speed, .. } | PadEffect::Starlight { speed, .. } => *speed,
        _ => 2,
    };
    match index {
        0 => PadEffect::Spectrum,
        1 => PadEffect::Wave(match old {
            PadEffect::Wave(d) => *d,
            _ => Travel::Left,
        }),
        2 => PadEffect::Breathing(colors),
        3 => PadEffect::Reactive {
            color: colors.first().copied().unwrap_or(Rgb(255, 255, 255)),
            speed: speed.min(REACTIVE_MAX_SPEED),
        },
        _ => PadEffect::Starlight {
            colors: colors.into_iter().take(2).collect(),
            speed: speed.min(STARLIGHT_MAX_SPEED),
        },
    }
}

/// Make a breathing or starlight effect use 0 (random), 1 or 2 colors.
pub fn set_color_count(e: &mut PadEffect, n: usize) {
    const DEFAULTS: [Rgb; 2] = [Rgb(255, 0, 0), Rgb(0, 0, 255)];
    if let PadEffect::Breathing(c) | PadEffect::Starlight { colors: c, .. } = e {
        c.truncate(n);
        while c.len() < n.min(2) {
            c.push(DEFAULTS[c.len()]);
        }
    }
}

pub fn set_default_color(p: &mut Profile, mode: usize, layer: Layer, c: Rgb) {
    if let Some(f) = p.modes.get_mut(mode).and_then(|m| frame_mut(m, layer)) {
        f.default = c;
    }
}

/// Set one key's color, or clear it (`None`) so it shows the frame's default.
pub fn set_color(p: &mut Profile, mode: usize, layer: Layer, key: Key, c: Option<Rgb>) {
    if let Some(f) = p.modes.get_mut(mode).and_then(|m| frame_mut(m, layer)) {
        match c {
            Some(c) => f.keys.insert(key, c),
            None => f.keys.remove(&key),
        };
    }
}

/// Set (or clear, with `None`) the color of several keys at once.
pub fn set_colors(p: &mut Profile, mode: usize, layer: Layer, keys: &[Key], c: Option<Rgb>) {
    for k in keys {
        set_color(p, mode, layer, *k, c);
    }
}

/// The color a key shows in this layer's frame, or `None` if the layer has no frame.
pub fn color_of(p: &Profile, mode: usize, layer: Layer, key: Key) -> Option<Rgb> {
    let f = frame(p, mode, layer)?;
    Some(f.keys.get(&key).copied().unwrap_or(f.default))
}

/// What to show on the pad while this layer is being edited.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Preview {
    /// A rendered 84-byte frame.
    Frame(Vec<u8>),
    /// A firmware effect as TOML.
    Effect(String),
}

pub fn preview(p: &Profile, mode: usize, layer: Layer) -> Option<Preview> {
    match light(p, mode, layer)? {
        Light::Frame(f) => Some(Preview::Frame(f.render().to_vec())),
        Light::Effect(e) => Some(Preview::Effect(e.to_toml_string())),
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
    fn one_action_can_be_applied_to_many_keys_and_a_common_one_is_found() {
        let mut p = blank("x");
        let keys = [k("KEY_1"), k("KEY_2"), k("KEY_3")];
        assert_eq!(
            common_binding(&p, 0, Layer::Base, &keys),
            Some(None),
            "all unbound"
        );
        assert_eq!(common_binding(&p, 0, Layer::Base, &[]), None);
        set_bindings(&mut p, 0, Layer::Base, &keys, Some(Action::Disabled));
        validate(&p).unwrap();
        for key in keys {
            assert_eq!(binding(&p, 0, Layer::Base, key), Some(&Action::Disabled));
        }
        assert_eq!(
            common_binding(&p, 0, Layer::Base, &keys),
            Some(Some(Action::Disabled))
        );
        set_binding(&mut p, 0, Layer::Base, keys[1], Some(Action::NextMode));
        assert_eq!(
            common_binding(&p, 0, Layer::Base, &keys),
            None,
            "they differ now"
        );
        set_bindings(&mut p, 0, Layer::Base, &keys, None);
        assert_eq!(common_binding(&p, 0, Layer::Base, &keys), Some(None));
        // Hypershift: an emptied layer disappears once every key is cleared.
        set_bindings(&mut p, 0, Layer::Hypershift, &keys, Some(Action::Disabled));
        assert!(p.modes[0].hypershift.is_some());
        set_bindings(&mut p, 0, Layer::Hypershift, &keys, None);
        assert!(p.modes[0].hypershift.is_none());
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
    fn colors_apply_to_many_keys_and_preview_renders_the_frame() {
        let mut p = blank("x");
        let keys: Vec<Key> = ["KEY_1", "KEY_2", "KEY_W"]
            .iter()
            .map(|n| Key::from_name(n).unwrap())
            .collect();
        assert_eq!(preview(&p, 0, Layer::Base), None);
        assert_eq!(color_of(&p, 0, Layer::Base, keys[0]), None);
        set_kind(&mut p, 0, Layer::Base, LightKind::Keys);
        set_default_color(&mut p, 0, Layer::Base, Rgb(1, 1, 1));
        set_colors(&mut p, 0, Layer::Base, &keys, Some(Rgb(9, 8, 7)));
        validate(&p).unwrap();
        assert_eq!(color_of(&p, 0, Layer::Base, keys[2]), Some(Rgb(9, 8, 7)));
        let Some(Preview::Frame(frame)) = preview(&p, 0, Layer::Base) else {
            panic!("a frame")
        };
        assert_eq!(frame.len(), tartarusd_core::lighting::FRAME_LEN);
        assert_eq!(&frame[3..6], [9, 8, 7], "KEY_1 is row 0, col 0");
        set_colors(&mut p, 0, Layer::Base, &keys[..2], None);
        assert_eq!(color_of(&p, 0, Layer::Base, keys[0]), Some(Rgb(1, 1, 1)));
        assert_eq!(color_of(&p, 0, Layer::Base, keys[2]), Some(Rgb(9, 8, 7)));
    }

    #[test]
    fn a_layer_has_key_colors_or_an_effect_and_switching_keeps_what_it_can() {
        let mut p = blank("x");
        assert_eq!(kind(&p, 0, Layer::Base), LightKind::Off);
        set_kind(&mut p, 0, Layer::Base, LightKind::Keys);
        set_default_color(&mut p, 0, Layer::Base, Rgb(5, 5, 5));
        set_kind(&mut p, 0, Layer::Base, LightKind::Keys);
        assert_eq!(
            frame(&p, 0, Layer::Base).unwrap().default,
            Rgb(5, 5, 5),
            "same kind keeps it"
        );
        set_kind(&mut p, 0, Layer::Base, LightKind::Effect);
        assert!(frame(&p, 0, Layer::Base).is_none());
        assert_eq!(effect(&p, 0, Layer::Base), Some(&PadEffect::Spectrum));
        validate(&p).unwrap();
        update_effect(&mut p, 0, Layer::Base, |e| {
            *e = PadEffect::Wave(Travel::Right)
        });
        assert!(matches!(
            preview(&p, 0, Layer::Base),
            Some(Preview::Effect(t)) if t.contains("wave") && t.contains("right")
        ));
        // Colors edits don't touch an effect layer.
        set_color(
            &mut p,
            0,
            Layer::Base,
            Key::from_name("KEY_1").unwrap(),
            Some(Rgb(1, 1, 1)),
        );
        assert_eq!(
            effect(&p, 0, Layer::Base),
            Some(&PadEffect::Wave(Travel::Right))
        );
        set_kind(&mut p, 0, Layer::Base, LightKind::Off);
        assert!(!p.uses_lighting());
    }

    #[test]
    fn copying_the_base_layer_dims_every_color_and_replaces_what_was_there() {
        let mut p = blank("x");
        let w = Key::from_name("KEY_W").unwrap();
        assert!(
            !copy_layer(&mut p, 0, Layer::Base, Layer::Hypershift, 50),
            "nothing to copy"
        );
        set_kind(&mut p, 0, Layer::Base, LightKind::Keys);
        set_default_color(&mut p, 0, Layer::Base, Rgb(100, 100, 100));
        set_color(&mut p, 0, Layer::Base, w, Some(Rgb(255, 0, 51)));
        // The Hypershift layer had something else; it is replaced.
        set_kind(&mut p, 0, Layer::Hypershift, LightKind::Effect);
        assert!(copy_layer(&mut p, 0, Layer::Base, Layer::Hypershift, 40));
        validate(&p).unwrap();
        let f = frame(&p, 0, Layer::Hypershift).unwrap();
        assert_eq!(f.default, Rgb(40, 40, 40));
        assert_eq!(
            f.keys[&w],
            Rgb(102, 0, 20),
            "every key scaled, each channel on its own"
        );
        assert_eq!(
            frame(&p, 0, Layer::Base).unwrap().default,
            Rgb(100, 100, 100),
            "the source is untouched"
        );
    }

    #[test]
    fn scaling_clamps_and_leaves_colorless_effects_alone() {
        assert_eq!(scale(Rgb(200, 100, 0), 200), Rgb(255, 200, 0));
        assert_eq!(scale(Rgb(255, 255, 255), 0), Rgb(0, 0, 0));
        assert_eq!(scale(Rgb(255, 255, 255), 100), Rgb(255, 255, 255));
        let wave = Light::Effect(PadEffect::Wave(Travel::Left));
        assert_eq!(scale_light(&wave, 30), wave);
        let star = Light::Effect(PadEffect::Starlight {
            colors: vec![Rgb(200, 0, 0)],
            speed: 3,
        });
        assert_eq!(
            scale_light(&star, 50),
            Light::Effect(PadEffect::Starlight {
                colors: vec![Rgb(100, 0, 0)],
                speed: 3
            })
        );
        let mut p = blank("x");
        set_kind(&mut p, 0, Layer::Base, LightKind::Keys);
        set_default_color(&mut p, 0, Layer::Base, Rgb(100, 100, 100));
        scale_layer(&mut p, 0, Layer::Base, 50);
        assert_eq!(frame(&p, 0, Layer::Base).unwrap().default, Rgb(50, 50, 50));
    }

    #[test]
    fn color_counts_truncate_and_extend_with_defaults() {
        let mut e = PadEffect::Breathing(vec![]);
        set_color_count(&mut e, 2);
        assert_eq!(
            e,
            PadEffect::Breathing(vec![Rgb(255, 0, 0), Rgb(0, 0, 255)])
        );
        set_color_count(&mut e, 1);
        assert_eq!(e, PadEffect::Breathing(vec![Rgb(255, 0, 0)]));
        set_color_count(&mut e, 0);
        assert_eq!(e, PadEffect::Breathing(vec![]));
        let mut s = PadEffect::Starlight {
            colors: vec![Rgb(9, 9, 9)],
            speed: 2,
        };
        set_color_count(&mut s, 2);
        assert_eq!(
            s,
            PadEffect::Starlight {
                colors: vec![Rgb(9, 9, 9), Rgb(0, 0, 255)],
                speed: 2
            }
        );
        let mut w = PadEffect::Wave(Travel::Left);
        set_color_count(&mut w, 2);
        assert_eq!(
            w,
            PadEffect::Wave(Travel::Left),
            "other effects have no colors list"
        );
    }

    #[test]
    fn switching_effect_type_carries_colors_and_clamps_speed() {
        let red = Rgb(255, 0, 0);
        let star = PadEffect::Starlight {
            colors: vec![red, Rgb(0, 0, 255)],
            speed: 3,
        };
        assert_eq!(
            with_effect_type(&star, 3),
            PadEffect::Reactive {
                color: red,
                speed: 3
            },
            "first color carried, speed kept"
        );
        let reactive = PadEffect::Reactive {
            color: red,
            speed: 4,
        };
        assert_eq!(
            with_effect_type(&reactive, 4),
            PadEffect::Starlight {
                colors: vec![red],
                speed: 3
            },
            "starlight tops out at 3"
        );
        assert_eq!(with_effect_type(&reactive, 0), PadEffect::Spectrum);
        assert_eq!(
            with_effect_type(&PadEffect::Spectrum, 3),
            PadEffect::Reactive {
                color: Rgb(255, 255, 255),
                speed: 2
            }
        );
        for (i, name) in EFFECT_TYPES.iter().enumerate() {
            let e = with_effect_type(&PadEffect::Spectrum, i as u32);
            assert_eq!(effect_type_index(&e), i as u32, "{name}");
        }
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
        set_kind(&mut p, 0, Layer::Base, LightKind::Keys);
        set_default_color(&mut p, 0, Layer::Base, Rgb(9, 9, 9));
        set_color(&mut p, 0, Layer::Base, w, Some(Rgb(1, 2, 3)));
        assert!(p.uses_lighting());
        validate(&p).unwrap();
        let f = frame(&p, 0, Layer::Base).unwrap();
        assert_eq!(f.keys[&w], Rgb(1, 2, 3));
        assert_eq!(f.default, Rgb(9, 9, 9));
        set_color(&mut p, 0, Layer::Base, w, None);
        assert!(frame(&p, 0, Layer::Base).unwrap().keys.is_empty());
        set_kind(&mut p, 0, Layer::Base, LightKind::Off);
        assert!(!p.uses_lighting());
    }
}
