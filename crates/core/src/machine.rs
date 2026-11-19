//! The pure remapping state machine. No I/O, no clock: callers pass `now` as a
//! monotonic offset and apply the returned [`Effect`]s.
//!
//! Invariants (tested, including a randomized interleaving test):
//! - Every emitted key-down has exactly one matching key-up, across mode switches,
//!   profile swaps, and [`Machine::release_all`]. Output keys are reference-counted, so
//!   two physical keys bound to the same output key emit one down and one up.
//! - Key-up releases what key-down recorded (`held`), never the current table.
//! - The mode key never produces output; tap vs hold is decided lazily.

use std::{collections::HashMap, sync::Arc, time::Duration};

use crate::{
    action::{Action, Step},
    config::Config,
    keys::{Edge, Key, PhysicalKey, WheelDir},
    profile::Profile,
};

/// One physical edge. Wheel notches are fed as `Press` immediately followed by
/// `Release`. Auto-repeat must be filtered by the caller (repeats are also ignored here).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Press(PhysicalKey),
    Release(PhysicalKey),
}

/// Side effects for the daemon to perform, in order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Emit a key edge on the virtual device.
    Key { key: Key, edge: Edge },
    /// Forward one wheel notch (`REL_WHEEL` and `REL_WHEEL_HI_RES`).
    Wheel(WheelDir),
    /// Run a timed macro (scheduled by the daemon).
    RunMacro(Arc<[Step]>),
    /// Show this profile-LED mask (B=1, G=2, R=4).
    SetLed(u8),
    /// The active mode changed (for D-Bus signals).
    ModeChanged(u8),
}

#[derive(Clone, Copy, Debug)]
enum ModeKeyState {
    Idle,
    /// Pressed at this time with no other input yet.
    Pending(Duration),
    /// Another input arrived while held: Hypershift layer is active.
    Shifted,
}

/// What to undo when a physical key is released.
#[derive(Debug)]
enum Held {
    Nothing,
    /// Output keys to release, in reverse order.
    Keys(Vec<Key>),
}

pub struct Machine {
    cfg: Config,
    profile: Arc<Profile>,
    mode: usize,
    mode_key: ModeKeyState,
    held: HashMap<PhysicalKey, Held>,
    /// Output keys currently down, with reference counts.
    out: HashMap<Key, u32>,
    remembered: HashMap<String, usize>,
}

impl Machine {
    pub fn new(cfg: Config, profile: Arc<Profile>) -> Machine {
        Machine {
            cfg,
            profile,
            mode: 0,
            mode_key: ModeKeyState::Idle,
            held: HashMap::new(),
            out: HashMap::new(),
            remembered: HashMap::new(),
        }
    }

    pub fn mode(&self) -> u8 {
        self.mode as u8
    }

    pub fn is_shifted(&self) -> bool {
        matches!(self.mode_key, ModeKeyState::Shifted)
    }

    pub fn profile(&self) -> &Arc<Profile> {
        &self.profile
    }

    /// The LED mask that should currently be displayed.
    pub fn led(&self) -> u8 {
        match (self.is_shifted(), self.cfg.hypershift_led_mask) {
            (true, Some(mask)) => mask,
            _ => self.profile.modes[self.mode].led_mask,
        }
    }

    pub fn handle(&mut self, now: Duration, ev: Event) -> Vec<Effect> {
        let mut fx = Vec::new();
        let mode_key = PhysicalKey::Key(self.cfg.mode_key);
        match ev {
            Event::Press(k) if k == mode_key => self.mode_key_down(now),
            Event::Release(k) if k == mode_key => self.mode_key_up(now, &mut fx),
            Event::Press(k) => self.press(k, &mut fx),
            Event::Release(k) => self.release(k, &mut fx),
        }
        fx
    }

    /// Jump to a mode (D-Bus `SetMode`). Out-of-range is ignored.
    pub fn set_mode(&mut self, n: u8) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.change_mode(usize::from(n), &mut fx);
        fx
    }

    /// Release every output key that is down. Used on shutdown, capture start, and
    /// profile swap. Physical keys still held stay "pressed": their later release is
    /// a no-op.
    pub fn release_all(&mut self) -> Vec<Effect> {
        let mut keys: Vec<Key> = self.out.keys().copied().collect();
        keys.sort();
        self.out.clear();
        self.held.clear();
        keys.into_iter()
            .map(|key| Effect::Key {
                key,
                edge: Edge::Up,
            })
            .collect()
    }

    /// Swap profiles: release everything, then restore the remembered mode (if the new
    /// profile opts in) or reset to mode 0. The mode-key state is deliberately kept, so
    /// a held mode key stays a held mode key.
    pub fn swap_profile(&mut self, profile: Arc<Profile>) -> Vec<Effect> {
        let mut fx = self.release_all();
        if self.profile.remember_mode {
            self.remembered.insert(self.profile.name.clone(), self.mode);
        }
        self.mode = if profile.remember_mode {
            self.remembered
                .get(&profile.name)
                .copied()
                .filter(|m| *m < profile.modes.len())
                .unwrap_or(0)
        } else {
            0
        };
        self.profile = profile;
        fx.push(Effect::ModeChanged(self.mode()));
        fx.push(Effect::SetLed(self.led()));
        fx
    }

    fn mode_key_down(&mut self, now: Duration) {
        if matches!(self.mode_key, ModeKeyState::Idle) {
            self.mode_key = ModeKeyState::Pending(now);
        }
    }

    fn mode_key_up(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        match std::mem::replace(&mut self.mode_key, ModeKeyState::Idle) {
            ModeKeyState::Pending(t0) => {
                if now.saturating_sub(t0) <= self.cfg.tap_term {
                    let next = (self.mode + 1) % self.profile.modes.len();
                    self.change_mode(next, fx);
                }
            }
            ModeKeyState::Shifted => {
                if self.cfg.hypershift_led_mask.is_some() {
                    fx.push(Effect::SetLed(self.led()));
                }
            }
            ModeKeyState::Idle => {}
        }
    }

    fn press(&mut self, pk: PhysicalKey, fx: &mut Vec<Effect>) {
        if self.held.contains_key(&pk) {
            return; // auto-repeat
        }
        if matches!(self.mode_key, ModeKeyState::Pending(_)) {
            self.mode_key = ModeKeyState::Shifted;
            if self.cfg.hypershift_led_mask.is_some() {
                fx.push(Effect::SetLed(self.led()));
            }
        }
        let action = self.lookup(pk);
        self.perform(pk, action, fx);
    }

    fn lookup(&self, pk: PhysicalKey) -> Action {
        let mode = &self.profile.modes[self.mode];
        let found = if self.is_shifted() {
            mode.hypershift
                .as_ref()
                .and_then(|layer| layer.get(&pk))
                .or_else(|| {
                    self.cfg
                        .hypershift_fallthrough
                        .then(|| mode.bindings.get(&pk))
                        .flatten()
                })
        } else {
            mode.bindings.get(&pk)
        };
        found.cloned().unwrap_or(Action::Passthrough)
    }

    fn perform(&mut self, pk: PhysicalKey, action: Action, fx: &mut Vec<Effect>) {
        let held = match action {
            Action::Passthrough => match pk {
                PhysicalKey::Key(k) => {
                    self.key_down(k, fx);
                    Held::Keys(vec![k])
                }
                PhysicalKey::Wheel(dir) => {
                    fx.push(Effect::Wheel(dir));
                    Held::Nothing
                }
            },
            Action::Disabled => Held::Nothing,
            Action::Chord(keys) => {
                for &k in &keys {
                    self.key_down(k, fx);
                }
                Held::Keys(keys)
            }
            Action::Macro(steps) => {
                fx.push(Effect::RunMacro(steps));
                Held::Nothing
            }
            Action::SetMode(n) => {
                self.change_mode(usize::from(n), fx);
                Held::Nothing
            }
            Action::NextMode => {
                self.change_mode((self.mode + 1) % self.profile.modes.len(), fx);
                Held::Nothing
            }
        };
        self.held.insert(pk, held);
    }

    fn release(&mut self, pk: PhysicalKey, fx: &mut Vec<Effect>) {
        // Release what was pressed, not what the current mode would press.
        if let Some(Held::Keys(keys)) = self.held.remove(&pk) {
            for k in keys.into_iter().rev() {
                self.key_up(k, fx);
            }
        }
    }

    fn change_mode(&mut self, n: usize, fx: &mut Vec<Effect>) {
        if n >= self.profile.modes.len() || n == self.mode {
            return;
        }
        self.mode = n;
        fx.push(Effect::ModeChanged(self.mode()));
        fx.push(Effect::SetLed(self.led()));
    }

    fn key_down(&mut self, key: Key, fx: &mut Vec<Effect>) {
        let n = self.out.entry(key).or_insert(0);
        *n += 1;
        if *n == 1 {
            fx.push(Effect::Key {
                key,
                edge: Edge::Down,
            });
        }
    }

    fn key_up(&mut self, key: Key, fx: &mut Vec<Effect>) {
        if let Some(n) = self.out.get_mut(&key) {
            *n -= 1;
            if *n == 0 {
                self.out.remove(&key);
                fx.push(Effect::Key {
                    key,
                    edge: Edge::Up,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn k(name: &str) -> Key {
        Key::from_name(name).unwrap()
    }
    fn pk(name: &str) -> PhysicalKey {
        PhysicalKey::Key(k(name))
    }
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
    fn down(name: &str) -> Effect {
        Effect::Key {
            key: k(name),
            edge: Edge::Down,
        }
    }
    fn up(name: &str) -> Effect {
        Effect::Key {
            key: k(name),
            edge: Edge::Up,
        }
    }

    const PROFILE: &str = r#"
name = "A"
[mode.0]
KEY_1 = "KEY_R"
KEY_2 = "KEY_LEFTCTRL+KEY_C"
KEY_3 = "disabled"
WHEEL_UP = "KEY_PAGEUP"
[mode.0.hypershift]
KEY_1 = "KEY_F1"
KEY_9 = { set_mode = 2 }
[mode.1]
KEY_1 = "KEY_T"
[mode.2]
"#;

    fn profile(src: &str) -> Arc<Profile> {
        Arc::new(Profile::from_toml(src).unwrap())
    }

    fn machine() -> Machine {
        Machine::new(Config::default(), profile(PROFILE))
    }

    fn press(m: &mut Machine, t: u64, name: &str) -> Vec<Effect> {
        m.handle(ms(t), Event::Press(pk(name)))
    }
    fn release(m: &mut Machine, t: u64, name: &str) -> Vec<Effect> {
        m.handle(ms(t), Event::Release(pk(name)))
    }

    #[test]
    fn tap_cycles_modes_with_led_and_no_output() {
        let mut m = machine();
        assert!(press(&mut m, 0, "KEY_LEFTALT").is_empty());
        let fx = release(&mut m, 100, "KEY_LEFTALT");
        assert_eq!(fx, vec![Effect::ModeChanged(1), Effect::SetLed(2)]);
        // Wraps after mode 2.
        for t in [1000, 2000] {
            press(&mut m, t, "KEY_LEFTALT");
            release(&mut m, t + 50, "KEY_LEFTALT");
        }
        assert_eq!(m.mode(), 0);
        assert_eq!(m.led(), 1);
    }

    #[test]
    fn long_press_with_no_other_input_is_a_noop() {
        let term = Config::default().tap_term.as_millis() as u64;
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        assert!(release(&mut m, term + 1, "KEY_LEFTALT").is_empty());
        assert_eq!(m.mode(), 0);
        // Exactly tap_term counts as a tap.
        press(&mut m, 10_000, "KEY_LEFTALT");
        assert!(!release(&mut m, 10_000 + term, "KEY_LEFTALT").is_empty());
    }

    #[test]
    fn hold_plus_key_is_hypershift_and_keeps_mode() {
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        assert_eq!(press(&mut m, 10, "KEY_1"), vec![down("KEY_F1")]);
        assert!(m.is_shifted());
        assert_eq!(release(&mut m, 20, "KEY_1"), vec![up("KEY_F1")]);
        // Releasing the mode key after a Hypershift use never changes the mode.
        assert!(release(&mut m, 30, "KEY_LEFTALT").is_empty());
        assert_eq!(m.mode(), 0);
        assert!(!m.is_shifted());
    }

    #[test]
    fn mode_key_released_first_still_releases_hypershift_binding() {
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        press(&mut m, 10, "KEY_1");
        release(&mut m, 20, "KEY_LEFTALT");
        assert_eq!(release(&mut m, 30, "KEY_1"), vec![up("KEY_F1")]);
    }

    #[test]
    fn release_uses_recorded_action_not_current_mode() {
        let mut m = machine();
        assert_eq!(press(&mut m, 0, "KEY_1"), vec![down("KEY_R")]);
        // Mode changes while KEY_1 is held (mode 1 maps KEY_1 to KEY_T).
        m.set_mode(1);
        assert_eq!(release(&mut m, 10, "KEY_1"), vec![up("KEY_R")]);
        assert_eq!(press(&mut m, 20, "KEY_1"), vec![down("KEY_T")]);
    }

    #[test]
    fn hypershift_set_mode_changes_mode() {
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        let fx = press(&mut m, 10, "KEY_9");
        assert_eq!(fx, vec![Effect::ModeChanged(2), Effect::SetLed(4)]);
        release(&mut m, 20, "KEY_9");
        release(&mut m, 30, "KEY_LEFTALT");
        assert_eq!(m.mode(), 2);
    }

    #[test]
    fn chord_presses_in_order_releases_in_reverse() {
        let mut m = machine();
        assert_eq!(
            press(&mut m, 0, "KEY_2"),
            vec![down("KEY_LEFTCTRL"), down("KEY_C")]
        );
        assert_eq!(
            release(&mut m, 5, "KEY_2"),
            vec![up("KEY_C"), up("KEY_LEFTCTRL")]
        );
    }

    #[test]
    fn passthrough_disabled_and_wheel() {
        let mut m = machine();
        assert_eq!(press(&mut m, 0, "KEY_Z"), vec![down("KEY_Z")]); // unmapped
        assert_eq!(release(&mut m, 1, "KEY_Z"), vec![up("KEY_Z")]);
        assert!(press(&mut m, 2, "KEY_3").is_empty());
        assert!(release(&mut m, 3, "KEY_3").is_empty());
        // Mapped wheel acts as a tap of the bound key.
        let w = PhysicalKey::Wheel(WheelDir::Up);
        assert_eq!(m.handle(ms(4), Event::Press(w)), vec![down("KEY_PAGEUP")]);
        assert_eq!(m.handle(ms(4), Event::Release(w)), vec![up("KEY_PAGEUP")]);
        // Unmapped wheel passes through as a wheel notch.
        let d = PhysicalKey::Wheel(WheelDir::Down);
        assert_eq!(
            m.handle(ms(5), Event::Press(d)),
            vec![Effect::Wheel(WheelDir::Down)]
        );
        assert!(m.handle(ms(5), Event::Release(d)).is_empty());
    }

    #[test]
    fn auto_repeat_is_ignored() {
        let mut m = machine();
        press(&mut m, 0, "KEY_1");
        assert!(press(&mut m, 1, "KEY_1").is_empty());
        assert_eq!(release(&mut m, 2, "KEY_1"), vec![up("KEY_R")]);
    }

    #[test]
    fn hypershift_fallthrough_toggle() {
        // KEY_2 is unmapped in the hypershift layer.
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        assert_eq!(
            press(&mut m, 1, "KEY_2"),
            vec![down("KEY_LEFTCTRL"), down("KEY_C")]
        );

        let cfg = Config {
            hypershift_fallthrough: false,
            ..Config::default()
        };
        let mut m = Machine::new(cfg, profile(PROFILE));
        press(&mut m, 0, "KEY_LEFTALT");
        assert_eq!(press(&mut m, 1, "KEY_2"), vec![down("KEY_2")]);
    }

    #[test]
    fn hypershift_led_shown_and_restored() {
        let cfg = Config {
            hypershift_led_mask: Some(7),
            ..Config::default()
        };
        let mut m = Machine::new(cfg, profile(PROFILE));
        press(&mut m, 0, "KEY_LEFTALT");
        assert_eq!(press(&mut m, 1, "KEY_1")[0], Effect::SetLed(7));
        release(&mut m, 2, "KEY_1");
        assert_eq!(release(&mut m, 3, "KEY_LEFTALT"), vec![Effect::SetLed(1)]);
    }

    #[test]
    fn shared_output_key_is_refcounted() {
        let src = "name='B'\n[mode.0]\nKEY_1='KEY_A'\nKEY_2='KEY_A'\n";
        let mut m = Machine::new(Config::default(), profile(src));
        assert_eq!(press(&mut m, 0, "KEY_1"), vec![down("KEY_A")]);
        assert!(press(&mut m, 1, "KEY_2").is_empty());
        assert!(release(&mut m, 2, "KEY_1").is_empty());
        assert_eq!(release(&mut m, 3, "KEY_2"), vec![up("KEY_A")]);
    }

    #[test]
    fn swap_releases_held_and_resets_mode() {
        let mut m = machine();
        m.set_mode(1);
        press(&mut m, 0, "KEY_1"); // KEY_T in mode 1
        let fx = m.swap_profile(profile("name='B'\n[mode.0]\n"));
        assert_eq!(fx[0], up("KEY_T"));
        assert_eq!(m.mode(), 0);
        // The physical release afterwards is a harmless no-op.
        assert!(release(&mut m, 5, "KEY_1").is_empty());
    }

    #[test]
    fn remember_mode_restores_per_profile() {
        let remember = "name='R'\nremember_mode=true\n[mode.0]\n[mode.1]\n[mode.2]\n";
        let mut m = Machine::new(Config::default(), profile(remember));
        m.set_mode(2);
        m.swap_profile(profile("name='Other'\n[mode.0]\n"));
        assert_eq!(m.mode(), 0);
        let fx = m.swap_profile(profile(remember));
        assert_eq!(m.mode(), 2);
        assert!(fx.contains(&Effect::SetLed(4)));
    }

    #[test]
    fn single_mode_profile_tap_is_a_noop() {
        let mut m = Machine::new(Config::default(), profile("name='S'\n[mode.0]\n"));
        press(&mut m, 0, "KEY_LEFTALT");
        assert!(release(&mut m, 10, "KEY_LEFTALT").is_empty());
    }

    #[test]
    fn mode_key_held_across_swap_stays_a_hold() {
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        m.swap_profile(profile(PROFILE));
        // Still Pending, so another key commits to Hypershift.
        assert_eq!(press(&mut m, 5, "KEY_1"), vec![down("KEY_F1")]);
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Random interleavings of keys, mode key, wheel, profile swaps and mode jumps must
    /// never double-press, double-release, leak the mode key, or leave a key stuck.
    #[test]
    fn fuzz_no_stuck_or_doubled_keys() {
        let other = profile(
            "name='O'\nextended_modes=true\n[mode.0]\nKEY_1='KEY_R'\nKEY_Q='KEY_R'\n\
             [mode.0.hypershift]\nKEY_2='KEY_LEFTSHIFT+KEY_A'\n[mode.1]\nKEY_Q='KEY_R+KEY_T'\n\
             [mode.2]\n[mode.3]\n",
        );
        let profiles = [profile(PROFILE), other];
        let inputs = [
            pk("KEY_LEFTALT"),
            pk("KEY_1"),
            pk("KEY_2"),
            pk("KEY_3"),
            pk("KEY_9"),
            pk("KEY_Q"),
            pk("KEY_Z"),
            PhysicalKey::Wheel(WheelDir::Up),
            PhysicalKey::Wheel(WheelDir::Down),
        ];
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let cfg = Config {
                hypershift_led_mask: (seed % 2 == 0).then_some(7),
                hypershift_fallthrough: seed % 3 != 0,
                ..Config::default()
            };
            let mut m = Machine::new(cfg, profiles[0].clone());
            let mut model: HashMap<Key, u32> = HashMap::new();
            let mut physical: HashSet<PhysicalKey> = HashSet::new();
            let mut now = 0u64;

            let apply = |fx: Vec<Effect>, model: &mut HashMap<Key, u32>| {
                for e in fx {
                    if let Effect::Key { key, edge } = e {
                        assert_ne!(key, Key::LEFTALT, "mode key leaked (seed {seed})");
                        match edge {
                            Edge::Down => {
                                assert_eq!(model.insert(key, 1), None, "double down (seed {seed})");
                            }
                            Edge::Up => {
                                assert!(model.remove(&key).is_some(), "stray up (seed {seed})");
                            }
                        }
                    }
                }
            };

            for _ in 0..400 {
                now += rng.next() % 400;
                let r = rng.next() % 40;
                let fx = if r == 0 {
                    m.swap_profile(profiles[(rng.next() % 2) as usize].clone())
                } else if r == 1 {
                    m.set_mode((rng.next() % 4) as u8)
                } else if r == 2 {
                    m.release_all()
                } else {
                    let input = inputs[(rng.next() % inputs.len() as u64) as usize];
                    if physical.remove(&input) {
                        m.handle(ms(now), Event::Release(input))
                    } else {
                        physical.insert(input);
                        m.handle(ms(now), Event::Press(input))
                    }
                };
                apply(fx, &mut model);
            }
            for input in physical {
                now += 1;
                let fx = m.handle(ms(now), Event::Release(input));
                apply(fx, &mut model);
            }
            assert!(model.is_empty(), "stuck keys {model:?} (seed {seed})");
        }
    }
}
