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
    config::{Config, ModeSwitch},
    keys::{Edge, Key, PhysicalKey, WheelDir},
    lighting::{Frame, Light, Pad},
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
    /// Show this profile-LED mask (B=1, G=2, R=4).
    SetLed(u8),
    /// The active mode changed (for D-Bus signals).
    ModeChanged(u8),
    /// Show this on the pad: a per-key frame, a firmware effect, or [`Pad::Handback`] if the
    /// profile does not use lighting. Emitted only when what is shown can change.
    SetPad(Pad),
    /// Hypershift was entered (`true`) or left (`false`). Emitted before any LED change.
    ShiftChanged(bool),
}

/// A held mode key whose Hypershift layer is active.
#[derive(Clone, Copy, Debug)]
struct Shift {
    /// When the key went down.
    since: Duration,
    /// Another input happened while it was held, so it is not a tap.
    used: bool,
    /// (`DoubleTap` only) this press followed a quick first tap.
    second: bool,
}

#[derive(Clone, Copy, Debug)]
enum ModeKeyState {
    Idle,
    /// (`Tap` only) pressed at this time with no other input yet.
    Pending(Duration),
    /// Hypershift layer is active.
    Shifted(Shift),
}

/// How long each phase of the blinking profile LED lasts while a mode is being selected.
const BLINK: Duration = Duration::from_millis(300);

/// (`Select` only) a timed mode selection: D-pad up/down move `candidate`, a single tap of
/// the mode key picks it, and silence cancels it.
#[derive(Clone, Copy, Debug)]
struct Select {
    candidate: usize,
    /// Cancelled when this passes; every navigation renews it.
    deadline: Duration,
    /// The LED is showing the candidate (otherwise it is in its dark phase).
    lit: bool,
    next_blink: Duration,
    /// The mode key is down, since this time.
    key_down: Option<Duration>,
}

/// A macro in flight. Its steps are performed by [`Machine::tick`].
struct RunningMacro {
    steps: Arc<[Step]>,
    /// Index of the next step to perform.
    next: usize,
    /// When that step is due.
    due: Duration,
}

/// Cap on concurrently running macros, so a stuck key or a flood of presses can't
/// grow without bound. Presses beyond the cap are ignored.
const MAX_RUNNING_MACROS: usize = 32;

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
    /// (`DoubleTap` only) a quick first tap just ended; a second tap before this time
    /// completes the double tap. Any other input in between cancels it.
    first_tap_until: Option<Duration>,
    select: Option<Select>,
    held: HashMap<PhysicalKey, Held>,
    /// Output keys currently down, with reference counts. Macro steps use the same
    /// counts as physical keys, so every down still has exactly one up.
    out: HashMap<Key, u32>,
    macros: Vec<RunningMacro>,
    remembered: HashMap<String, usize>,
}

impl Machine {
    pub fn new(cfg: Config, profile: Arc<Profile>) -> Machine {
        Machine {
            cfg,
            profile,
            mode: 0,
            mode_key: ModeKeyState::Idle,
            first_tap_until: None,
            select: None,
            held: HashMap::new(),
            out: HashMap::new(),
            macros: Vec::new(),
            remembered: HashMap::new(),
        }
    }

    pub fn mode(&self) -> u8 {
        self.mode as u8
    }

    pub fn is_shifted(&self) -> bool {
        matches!(self.mode_key, ModeKeyState::Shifted(_))
    }

    /// Whether a timed mode selection is open.
    pub fn is_selecting(&self) -> bool {
        self.select.is_some()
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

    /// What the pad should show now. A profile that uses no lighting at all hands the pad
    /// back. Otherwise a mode with nothing of its own shows a dark frame (the profile has
    /// taken over the pad), and Hypershift without its own lighting keeps the mode's.
    pub fn current_pad(&self) -> Pad {
        if !self.profile.uses_lighting() {
            return Pad::Handback;
        }
        let mode = &self.profile.modes[self.mode];
        let light = if self.is_shifted() {
            mode.hypershift_lighting.as_ref().or(mode.lighting.as_ref())
        } else {
            mode.lighting.as_ref()
        };
        match light {
            Some(Light::Effect(e)) => Pad::Effect(e.clone()),
            Some(Light::Frame(f)) => Pad::Frame(Arc::new(f.render())),
            None => Pad::Frame(Arc::new(Frame::default().render())),
        }
    }

    pub fn handle(&mut self, now: Duration, ev: Event) -> Vec<Effect> {
        let mut fx = Vec::new();
        let mode_key = PhysicalKey::Key(self.cfg.mode_key);
        if self.select.is_some() {
            self.select_event(now, ev, &mut fx);
            return fx;
        }
        match ev {
            Event::Press(k) if k == mode_key => self.mode_key_down(now, &mut fx),
            Event::Release(k) if k == mode_key => self.mode_key_up(now, &mut fx),
            Event::Press(k) => self.press(now, k, &mut fx),
            Event::Release(k) => self.release(k, &mut fx),
        }
        fx
    }

    /// Jump to a mode (D-Bus `SetMode`). Out-of-range is ignored.
    pub fn set_mode(&mut self, n: u8) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.end_select(&mut fx);
        self.change_mode(usize::from(n), &mut fx);
        fx
    }

    /// Cancel running macros and release every output key that is down. Used on shutdown, capture start, and
    /// profile swap. Physical keys still held stay "pressed": their later release is
    /// a no-op.
    pub fn release_all(&mut self) -> Vec<Effect> {
        // Cancel running macros first: their remaining steps must not fire afterwards.
        self.macros.clear();
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
        self.select = None;
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
        fx.push(Effect::SetPad(self.current_pad()));
        fx
    }

    /// Perform whatever is due at `now`: the mode key's promotion to Hypershift, and
    /// macro steps. Call when [`Machine::next_deadline`] arrives. Late macro steps run in
    /// order, each chained from `now`.
    pub fn tick(&mut self, now: Duration) -> Vec<Effect> {
        let mut fx = Vec::new();
        if let ModeKeyState::Pending(t0) = self.mode_key
            && now.saturating_sub(t0) >= self.cfg.tap_term
        {
            // Held past the tap term with nothing else pressed: it is a hold, so show
            // Hypershift now rather than waiting for another key.
            self.begin_shift(
                Shift {
                    since: t0,
                    used: true,
                    second: false,
                },
                &mut fx,
            );
        }
        if let Some(sel) = &mut self.select {
            if now >= sel.deadline {
                self.end_select(&mut fx);
            } else if now >= sel.next_blink {
                sel.lit = !sel.lit;
                sel.next_blink = now + BLINK;
                let mask = if sel.lit {
                    self.profile.modes[sel.candidate].led_mask
                } else {
                    0
                };
                fx.push(Effect::SetLed(mask));
            }
        }
        self.run_due(now, &mut fx);
        fx
    }

    /// When [`Machine::tick`] next has something to do: the mode key reaching the tap
    /// term, the next blink or selection timeout, or the next macro step.
    pub fn next_deadline(&self) -> Option<Duration> {
        let promotion = match self.mode_key {
            ModeKeyState::Pending(t0) => Some(t0 + self.cfg.tap_term),
            _ => None,
        };
        let select = self.select.map(|s| s.next_blink.min(s.deadline));
        self.macros
            .iter()
            .map(|m| m.due)
            .chain(promotion)
            .chain(select)
            .min()
    }

    fn start_macro(&mut self, now: Duration, steps: Arc<[Step]>, fx: &mut Vec<Effect>) {
        if self.macros.len() >= MAX_RUNNING_MACROS || steps.is_empty() {
            return;
        }
        let due = now + Duration::from_millis(u64::from(steps[0].delay_ms));
        self.macros.push(RunningMacro {
            steps,
            next: 0,
            due,
        });
        // A zero-delay first step happens now: macros add no latency of their own.
        self.run_due(now, fx);
    }

    fn run_due(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        let mut i = 0;
        while i < self.macros.len() {
            let mut finished = false;
            while !finished && self.macros[i].due <= now {
                let m = &mut self.macros[i];
                let (key, edge) = (m.steps[m.next].key, m.steps[m.next].edge);
                m.next += 1;
                if m.next < m.steps.len() {
                    m.due = now + Duration::from_millis(u64::from(m.steps[m.next].delay_ms));
                } else {
                    finished = true;
                }
                match edge {
                    Edge::Down => self.key_down(key, fx),
                    Edge::Up => self.key_up(key, fx),
                }
            }
            if finished {
                self.macros.remove(i);
            } else {
                i += 1;
            }
        }
    }

    fn mode_key_down(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        if !matches!(self.mode_key, ModeKeyState::Idle) {
            return; // repeat
        }
        match self.cfg.mode_switch {
            ModeSwitch::Tap => self.mode_key = ModeKeyState::Pending(now),
            ModeSwitch::DoubleTap | ModeSwitch::Select => {
                // Hypershift shows at once. Whether this press is also a tap is decided
                // on release.
                let second = self.first_tap_until.take().is_some_and(|until| now < until);
                self.begin_shift(
                    Shift {
                        since: now,
                        used: false,
                        second,
                    },
                    fx,
                );
            }
        }
    }

    fn mode_key_up(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        match std::mem::replace(&mut self.mode_key, ModeKeyState::Idle) {
            ModeKeyState::Pending(t0) => {
                // Strictly shorter than the tap term is a tap; at or beyond it the key is a
                // hold (whether or not a tick has promoted it yet).
                if now.saturating_sub(t0) < self.cfg.tap_term {
                    let next = (self.mode + 1) % self.profile.modes.len();
                    self.change_mode(next, fx);
                }
            }
            ModeKeyState::Shifted(s) => {
                fx.push(Effect::ShiftChanged(false));
                if self.cfg.hypershift_led_mask.is_some() {
                    fx.push(Effect::SetLed(self.led()));
                }
                self.push_shift_frame(fx);
                // A quick press-and-release with nothing else pressed is a tap. In
                // `DoubleTap` mode the first arms the second; the second cycles the mode.
                let quick = now.saturating_sub(s.since) < self.cfg.tap_term;
                if self.cfg.mode_switch != ModeSwitch::Tap && !s.used && quick {
                    if s.second && self.cfg.mode_switch == ModeSwitch::Select {
                        self.begin_select(now, fx);
                    } else if s.second {
                        let next = (self.mode + 1) % self.profile.modes.len();
                        self.change_mode(next, fx);
                    } else {
                        self.first_tap_until = Some(now + self.cfg.tap_term);
                    }
                }
            }
            ModeKeyState::Idle => {}
        }
    }

    /// Open the timed selection, starting on the current mode. A profile with one mode has
    /// nothing to choose, so the double tap does nothing there.
    fn begin_select(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        if self.profile.modes.len() < 2 {
            return;
        }
        self.select = Some(Select {
            candidate: self.mode,
            deadline: now + self.cfg.select_timeout,
            lit: true,
            next_blink: now + BLINK,
            key_down: None,
        });
        fx.push(Effect::SetLed(self.led()));
    }

    /// Close the selection, if open, and show the real mode's LED again.
    fn end_select(&mut self, fx: &mut Vec<Effect>) {
        if self.select.take().is_some() {
            fx.push(Effect::SetLed(self.led()));
        }
    }

    /// While selecting, input steers the selection and nothing reaches the output.
    fn select_event(&mut self, now: Duration, ev: Event, fx: &mut Vec<Effect>) {
        let mode_key = PhysicalKey::Key(self.cfg.mode_key);
        let n = self.profile.modes.len();
        let Some(sel) = &mut self.select else { return };
        match ev {
            Event::Press(k) if k == mode_key => sel.key_down = Some(now),
            Event::Release(k) if k == mode_key => {
                let quick = sel
                    .key_down
                    .take()
                    .is_some_and(|t| now.saturating_sub(t) < self.cfg.tap_term);
                if quick {
                    let pick = sel.candidate;
                    self.select = None;
                    if pick == self.mode {
                        fx.push(Effect::SetLed(self.led()));
                    } else {
                        self.change_mode(pick, fx);
                    }
                }
            }
            Event::Press(k) => {
                let step = if k == PhysicalKey::Key(Key::UP) {
                    n - 1
                } else if k == PhysicalKey::Key(Key::DOWN) {
                    1
                } else {
                    return;
                };
                sel.candidate = (sel.candidate + step) % n;
                sel.deadline = now + self.cfg.select_timeout;
                sel.lit = true;
                sel.next_blink = now + BLINK;
                fx.push(Effect::SetLed(self.profile.modes[sel.candidate].led_mask));
            }
            Event::Release(_) => {}
        }
    }

    fn press(&mut self, now: Duration, pk: PhysicalKey, fx: &mut Vec<Effect>) {
        if self.held.contains_key(&pk) {
            return; // auto-repeat
        }
        match &mut self.mode_key {
            // First other input while the key is undecided: commit to Hypershift.
            ModeKeyState::Pending(_) => {
                self.begin_shift(
                    Shift {
                        since: now,
                        used: true,
                        second: false,
                    },
                    fx,
                );
            }
            // Another input during a hold: it is a Hypershift use, not a tap.
            ModeKeyState::Shifted(s) => s.used = true,
            // Anything between two taps breaks the double tap.
            ModeKeyState::Idle => self.first_tap_until = None,
        }
        let action = self.lookup(pk);
        self.perform(now, pk, action, fx);
    }

    /// Enter Hypershift, with its visible effects (signal, LED, frame).
    fn begin_shift(&mut self, shift: Shift, fx: &mut Vec<Effect>) {
        self.mode_key = ModeKeyState::Shifted(shift);
        fx.push(Effect::ShiftChanged(true));
        if self.cfg.hypershift_led_mask.is_some() {
            fx.push(Effect::SetLed(self.led()));
        }
        self.push_shift_frame(fx);
    }

    /// On entering or leaving Hypershift, update the frame only if this mode has a
    /// Hypershift frame (otherwise nothing visible changes).
    fn push_shift_frame(&self, fx: &mut Vec<Effect>) {
        if self.profile.modes[self.mode].hypershift_lighting.is_some() {
            fx.push(Effect::SetPad(self.current_pad()));
        }
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

    fn perform(&mut self, now: Duration, pk: PhysicalKey, action: Action, fx: &mut Vec<Effect>) {
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
                self.start_macro(now, steps, fx);
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
        if self.profile.uses_lighting() {
            fx.push(Effect::SetPad(self.current_pad()));
        }
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
    use crate::lighting::Payload;

    /// The single-tap gesture (the pre-`mode_switch` behavior) that most tests below cover.
    fn tap_cfg() -> Config {
        Config {
            mode_switch: ModeSwitch::Tap,
            ..Config::default()
        }
    }

    /// The default gesture: Hypershift on press, double tap to cycle modes.
    fn dt_machine() -> Machine {
        Machine::new(Config::default(), profile(PROFILE))
    }

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
KEY_X = { macro = [["KEY_LEFTSHIFT", "down", 0], ["KEY_A", 30], ["KEY_LEFTSHIFT", "up", 30]] }
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
        Machine::new(tap_cfg(), profile(PROFILE))
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
        let term = tap_cfg().tap_term.as_millis() as u64;
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        assert!(release(&mut m, term + 1, "KEY_LEFTALT").is_empty());
        assert_eq!(m.mode(), 0);
        // One millisecond under the tap term is a tap; the tap term itself is a hold.
        press(&mut m, 10_000, "KEY_LEFTALT");
        assert!(!release(&mut m, 10_000 + term - 1, "KEY_LEFTALT").is_empty());
        press(&mut m, 20_000, "KEY_LEFTALT");
        assert!(release(&mut m, 20_000 + term, "KEY_LEFTALT").is_empty());
    }

    #[test]
    fn hold_plus_key_is_hypershift_and_keeps_mode() {
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        assert_eq!(
            press(&mut m, 10, "KEY_1"),
            vec![Effect::ShiftChanged(true), down("KEY_F1")]
        );
        assert!(m.is_shifted());
        assert_eq!(release(&mut m, 20, "KEY_1"), vec![up("KEY_F1")]);
        // Releasing the mode key after a Hypershift use never changes the mode.
        assert_eq!(
            release(&mut m, 30, "KEY_LEFTALT"),
            vec![Effect::ShiftChanged(false)]
        );
        assert_eq!(m.mode(), 0);
        assert!(!m.is_shifted());
    }

    fn term_ms() -> u64 {
        tap_cfg().tap_term.as_millis() as u64
    }

    #[test]
    fn holding_past_the_tap_term_enters_hypershift_without_another_key() {
        let term = term_ms();
        let mut m = machine();
        assert!(press(&mut m, 100, "KEY_LEFTALT").is_empty());
        assert_eq!(m.next_deadline(), Some(ms(100 + term)));
        assert!(m.tick(ms(100 + term - 1)).is_empty(), "still undecided");
        assert!(!m.is_shifted());

        assert_eq!(m.tick(ms(100 + term)), vec![Effect::ShiftChanged(true)]);
        assert!(m.is_shifted());
        assert_eq!(m.next_deadline(), None, "promotion happens once");

        // Another key now uses the Hypershift layer and does not re-announce the shift.
        assert_eq!(
            press(&mut m, 100 + term + 50, "KEY_1"),
            vec![down("KEY_F1")]
        );
        release(&mut m, 100 + term + 60, "KEY_1");

        // Releasing after a promoted hold leaves the mode alone, like any Hypershift use.
        assert_eq!(
            release(&mut m, 100 + term + 70, "KEY_LEFTALT"),
            vec![Effect::ShiftChanged(false)]
        );
        assert_eq!(m.mode(), 0);
    }

    #[test]
    fn promoted_hold_released_alone_is_not_a_tap() {
        let term = term_ms();
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        m.tick(ms(term));
        assert_eq!(
            release(&mut m, term + 10, "KEY_LEFTALT"),
            vec![Effect::ShiftChanged(false)]
        );
        assert_eq!(m.mode(), 0, "no mode change from a hold");
    }

    #[test]
    fn a_tap_never_promotes() {
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        assert!(release(&mut m, 50, "KEY_LEFTALT").contains(&Effect::ModeChanged(1)));
        assert_eq!(
            m.next_deadline(),
            None,
            "no pending promotion after release"
        );
        assert!(m.tick(ms(10_000)).is_empty());
    }

    #[test]
    fn promotion_shows_the_hypershift_led_and_frame() {
        let term = term_ms();
        let cfg = Config {
            hypershift_led_mask: Some(7),
            ..tap_cfg()
        };
        let mut m = Machine::new(cfg, profile(LIT));
        press(&mut m, 0, "KEY_LEFTALT");
        let fx = m.tick(ms(term));
        assert_eq!(fx[..2], [Effect::ShiftChanged(true), Effect::SetLed(7)]);
        assert_eq!(w_cell(frames(&fx)[0].as_ref().unwrap()), [16, 16, 16]);
        let back = release(&mut m, term + 5, "KEY_LEFTALT");
        assert_eq!(back[..2], [Effect::ShiftChanged(false), Effect::SetLed(1)]);
        assert_eq!(w_cell(frames(&back)[0].as_ref().unwrap()), [0, 255, 255]);
    }

    #[test]
    fn release_at_the_tap_term_without_a_tick_is_still_a_hold() {
        let term = term_ms();
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        assert!(release(&mut m, term, "KEY_LEFTALT").is_empty());
        assert_eq!(m.mode(), 0);
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
        assert_eq!(
            fx,
            vec![
                Effect::ShiftChanged(true),
                Effect::ModeChanged(2),
                Effect::SetLed(4)
            ]
        );
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
            vec![
                Effect::ShiftChanged(true),
                down("KEY_LEFTCTRL"),
                down("KEY_C")
            ]
        );

        let cfg = Config {
            hypershift_fallthrough: false,
            ..tap_cfg()
        };
        let mut m = Machine::new(cfg, profile(PROFILE));
        press(&mut m, 0, "KEY_LEFTALT");
        assert_eq!(
            press(&mut m, 1, "KEY_2"),
            vec![Effect::ShiftChanged(true), down("KEY_2")]
        );
    }

    #[test]
    fn hypershift_led_shown_and_restored() {
        let cfg = Config {
            hypershift_led_mask: Some(7),
            ..tap_cfg()
        };
        let mut m = Machine::new(cfg, profile(PROFILE));
        press(&mut m, 0, "KEY_LEFTALT");
        assert_eq!(
            press(&mut m, 1, "KEY_1")[..2],
            [Effect::ShiftChanged(true), Effect::SetLed(7)]
        );
        release(&mut m, 2, "KEY_1");
        assert_eq!(
            release(&mut m, 3, "KEY_LEFTALT"),
            vec![Effect::ShiftChanged(false), Effect::SetLed(1)]
        );
    }

    #[test]
    fn shared_output_key_is_refcounted() {
        let src = "name='B'\n[mode.0]\nKEY_1='KEY_A'\nKEY_2='KEY_A'\n";
        let mut m = Machine::new(tap_cfg(), profile(src));
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
        let mut m = Machine::new(tap_cfg(), profile(remember));
        m.set_mode(2);
        m.swap_profile(profile("name='Other'\n[mode.0]\n"));
        assert_eq!(m.mode(), 0);
        let fx = m.swap_profile(profile(remember));
        assert_eq!(m.mode(), 2);
        assert!(fx.contains(&Effect::SetLed(4)));
    }

    #[test]
    fn single_mode_profile_tap_is_a_noop() {
        let mut m = Machine::new(tap_cfg(), profile("name='S'\n[mode.0]\n"));
        press(&mut m, 0, "KEY_LEFTALT");
        assert!(release(&mut m, 10, "KEY_LEFTALT").is_empty());
    }

    #[test]
    fn mode_key_held_across_swap_stays_a_hold() {
        let mut m = machine();
        press(&mut m, 0, "KEY_LEFTALT");
        m.swap_profile(profile(PROFILE));
        // Still Pending, so another key commits to Hypershift.
        assert_eq!(
            press(&mut m, 5, "KEY_1"),
            vec![Effect::ShiftChanged(true), down("KEY_F1")]
        );
    }

    #[test]
    fn shift_changed_only_on_real_hypershift() {
        let mut m = machine();
        // A plain tap or a long press never reports a shift.
        press(&mut m, 0, "KEY_LEFTALT");
        assert!(!release(&mut m, 50, "KEY_LEFTALT").contains(&Effect::ShiftChanged(false)));
        // Auto-repeat of the mode key and a second shifted key report it once.
        press(&mut m, 1000, "KEY_LEFTALT");
        let a = press(&mut m, 1010, "KEY_1");
        let b = press(&mut m, 1020, "KEY_2");
        assert!(a.contains(&Effect::ShiftChanged(true)));
        assert!(!b.contains(&Effect::ShiftChanged(true)));
    }

    const MACROS: &str = r#"
name = "M"
[mode.0]
KEY_Q = { macro = [["KEY_E", 0], ["KEY_F", 50]] }
KEY_W = { macro = [["KEY_LEFTSHIFT", "down", 0], ["KEY_A", 30], ["KEY_LEFTSHIFT", "up", 30]] }
KEY_E = { macro = [["KEY_A", 100]] }
"#;

    fn macro_machine() -> Machine {
        Machine::new(tap_cfg(), profile(MACROS))
    }

    #[test]
    fn macro_first_step_is_immediate_rest_wait_for_tick() {
        let mut m = macro_machine();
        // Delay 0: the first tap happens inside the press, adding no latency.
        assert_eq!(press(&mut m, 0, "KEY_Q"), vec![down("KEY_E"), up("KEY_E")]);
        assert_eq!(m.next_deadline(), Some(ms(50)));
        assert!(m.tick(ms(49)).is_empty());
        assert_eq!(m.tick(ms(50)), vec![down("KEY_F"), up("KEY_F")]);
        assert_eq!(m.next_deadline(), None, "finished macros are dropped");
        // Releasing the physical key never affects a macro.
        assert!(release(&mut m, 60, "KEY_Q").is_empty());
    }

    #[test]
    fn late_tick_catches_up_in_order_and_chains_from_now() {
        let mut m = macro_machine();
        assert_eq!(press(&mut m, 0, "KEY_W"), vec![down("KEY_LEFTSHIFT")]);
        // The tick arrives far later than the 30 ms step: it still runs in order, and the
        // next delay counts from now (no burst of back-to-back steps).
        assert_eq!(m.tick(ms(500)), vec![down("KEY_A"), up("KEY_A")]);
        assert_eq!(m.next_deadline(), Some(ms(530)));
        assert_eq!(m.tick(ms(530)), vec![up("KEY_LEFTSHIFT")]);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn swapping_profiles_cancels_macros_and_releases_their_keys() {
        let mut m = macro_machine();
        press(&mut m, 0, "KEY_W"); // Shift is now down, rest pending
        let fx = m.swap_profile(profile("name='B'\n[mode.0]\n"));
        assert_eq!(fx[0], up("KEY_LEFTSHIFT"));
        assert!(
            m.tick(ms(10_000)).is_empty(),
            "cancelled macro must not fire"
        );
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn release_all_cancels_macros() {
        let mut m = macro_machine();
        press(&mut m, 0, "KEY_Q");
        assert!(m.next_deadline().is_some());
        m.release_all();
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn running_macros_are_capped() {
        let mut m = macro_machine();
        for t in 0..40 {
            press(&mut m, t, "KEY_E");
            release(&mut m, t, "KEY_E");
        }
        // Each macro taps A once at 140 ms+; only MAX_RUNNING_MACROS were accepted.
        let fx = m.tick(ms(10_000));
        let downs = fx.iter().filter(|e| **e == down("KEY_A")).count();
        assert_eq!(downs, MAX_RUNNING_MACROS);
    }

    const LIT: &str = r##"
name = "L"
[mode.0]
KEY_1 = "KEY_A"
[mode.0.lighting]
default = "#000000"
KEY_W = "#00ffff"
[mode.0.hypershift_lighting]
default = "#101010"
[mode.1]
[mode.1.lighting]
default = "#0000ff"
[mode.2]
"##;

    /// The pad changes in `fx`, as frames (`None` = hand the pad back).
    fn frames(fx: &[Effect]) -> Vec<Option<Arc<Payload>>> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::SetPad(Pad::Frame(f)) => Some(Some(f.clone())),
                Effect::SetPad(Pad::Handback) => Some(None),
                Effect::SetPad(Pad::Effect(_)) => panic!("unexpected effect"),
                _ => None,
            })
            .collect()
    }

    /// First color byte of a rendered frame's cell (row 1, col 2 = KEY_W).
    fn w_cell(p: &Payload) -> [u8; 3] {
        [p[21 + 3 + 6], p[21 + 3 + 7], p[21 + 3 + 8]]
    }

    #[test]
    fn frames_follow_mode_and_hypershift() {
        let mut m = Machine::new(tap_cfg(), profile(LIT));
        assert_eq!(
            w_cell(&m.current_pad().frame().unwrap().clone()),
            [0, 255, 255]
        );

        // Tap: mode 1 shows its own frame (default blue, KEY_W not overridden).
        press(&mut m, 0, "KEY_LEFTALT");
        let f = frames(&release(&mut m, 50, "KEY_LEFTALT"));
        assert_eq!(w_cell(f[0].as_ref().unwrap()), [0, 0, 255]);

        // Back to mode 0, then Hypershift: its frame, then restore on release.
        m.set_mode(0);
        press(&mut m, 1000, "KEY_LEFTALT");
        let enter = frames(&press(&mut m, 1010, "KEY_1"));
        assert_eq!(w_cell(enter[0].as_ref().unwrap()), [16, 16, 16]);
        release(&mut m, 1020, "KEY_1");
        let leave = frames(&release(&mut m, 1030, "KEY_LEFTALT"));
        assert_eq!(w_cell(leave[0].as_ref().unwrap()), [0, 255, 255]);
    }

    #[test]
    fn firmware_effects_follow_mode_and_hypershift_and_hand_back_when_absent() {
        use crate::lighting::{PadEffect, Travel};
        let text = r##"
name = "E"
[mode.0]
[mode.0.effect]
type = "wave"
direction = "right"
[mode.0.hypershift_lighting]
default = "#101010"
[mode.1]
[mode.1.effect]
type = "spectrum"
[mode.2]
"##;
        let mut m = Machine::new(tap_cfg(), profile(text));
        assert_eq!(m.current_pad(), Pad::Effect(PadEffect::Wave(Travel::Right)));
        let fx = m.set_mode(1);
        assert!(fx.contains(&Effect::SetPad(Pad::Effect(PadEffect::Spectrum))));
        // A mode with nothing of its own is dark: the profile owns the pad.
        let fx = m.set_mode(2);
        assert!(matches!(
            fx.iter().find_map(|e| match e {
                Effect::SetPad(p) => Some(p),
                _ => None,
            }),
            Some(Pad::Frame(_))
        ));
    }

    #[test]
    fn mode_without_a_frame_goes_dark_and_hypershift_keeps_the_mode_frame() {
        let mut m = Machine::new(tap_cfg(), profile(LIT));
        m.set_mode(2); // lighting profile, but mode 2 has no frame
        assert_eq!(w_cell(&m.current_pad().frame().unwrap().clone()), [0, 0, 0]);

        // Mode 1 has no hypershift_lighting: entering Hypershift changes no frame.
        m.set_mode(1);
        press(&mut m, 0, "KEY_LEFTALT");
        assert!(frames(&press(&mut m, 10, "KEY_1")).is_empty());
    }

    #[test]
    fn profiles_without_lighting_never_emit_frames_except_on_swap() {
        let mut m = machine(); // PROFILE has no lighting
        assert_eq!(m.current_pad(), Pad::Handback);
        assert!(frames(&m.set_mode(1)).is_empty());
        let swap = frames(&m.swap_profile(profile(LIT)));
        assert!(swap[0].is_some());
        let back = frames(&m.swap_profile(profile(PROFILE)));
        assert_eq!(
            back,
            vec![None],
            "hand the pad back on switching to an unlit profile"
        );
    }

    fn quick_tap(m: &mut Machine, t: u64, hold: u64) -> Vec<Effect> {
        press(m, t, "KEY_LEFTALT");
        release(m, t + hold, "KEY_LEFTALT")
    }

    #[test]
    fn double_tap_default_shows_hypershift_on_press_with_no_delay() {
        let mut m = dt_machine();
        assert_eq!(
            press(&mut m, 0, "KEY_LEFTALT"),
            vec![Effect::ShiftChanged(true)]
        );
        assert!(m.is_shifted());
        assert_eq!(m.next_deadline(), None, "nothing to wait for");
        // Other keys use the Hypershift layer straight away, without re-announcing it.
        assert_eq!(press(&mut m, 10, "KEY_1"), vec![down("KEY_F1")]);
        release(&mut m, 20, "KEY_1");
        assert_eq!(
            release(&mut m, 30, "KEY_LEFTALT"),
            vec![Effect::ShiftChanged(false)]
        );
        assert_eq!(m.mode(), 0);
    }

    #[test]
    fn a_single_tap_never_changes_the_mode() {
        let mut m = dt_machine();
        assert_eq!(quick_tap(&mut m, 0, 50), vec![Effect::ShiftChanged(false)]);
        assert_eq!(m.mode(), 0);
        // Even long after: the pending first tap just expires.
        assert_eq!(
            quick_tap(&mut m, 5_000, 50),
            vec![Effect::ShiftChanged(false)]
        );
        assert_eq!(m.mode(), 0);
    }

    #[test]
    fn two_quick_taps_cycle_the_mode() {
        let mut m = dt_machine();
        quick_tap(&mut m, 0, 50);
        let fx = quick_tap(&mut m, 150, 50);
        assert_eq!(
            fx,
            vec![
                Effect::ShiftChanged(false),
                Effect::ModeChanged(1),
                Effect::SetLed(2)
            ]
        );
        assert_eq!(m.mode(), 1);
        // And a double tap again advances (the first tap of the next pair is fresh).
        quick_tap(&mut m, 1_000, 50);
        quick_tap(&mut m, 1_100, 50);
        assert_eq!(m.mode(), 2);
    }

    #[test]
    fn the_gap_between_taps_must_be_shorter_than_the_tap_term() {
        let term = term_ms();
        let mut m = dt_machine();
        quick_tap(&mut m, 0, 50); // first tap ends at 50
        quick_tap(&mut m, 50 + term, 50); // gap == tap term: too slow, starts over
        assert_eq!(m.mode(), 0);
        // That second press counted as a new first tap, so the next quick one completes.
        quick_tap(&mut m, 50 + term + 50 + 100, 50);
        assert_eq!(m.mode(), 1);
    }

    #[test]
    fn each_tap_must_be_shorter_than_the_tap_term() {
        let term = term_ms();
        let mut m = dt_machine();
        quick_tap(&mut m, 0, 50);
        quick_tap(&mut m, 100, term); // second press held too long: a hold, not a tap
        assert_eq!(m.mode(), 0);
        // A slow first press doesn't arm anything either.
        quick_tap(&mut m, 10_000, term);
        quick_tap(&mut m, 10_000 + term + 50, 50);
        assert_eq!(m.mode(), 0);
    }

    #[test]
    fn any_other_input_between_or_during_taps_cancels_the_double_tap() {
        let mut m = dt_machine();
        // Between the taps.
        quick_tap(&mut m, 0, 50);
        press(&mut m, 100, "KEY_Z");
        release(&mut m, 110, "KEY_Z");
        quick_tap(&mut m, 150, 50);
        assert_eq!(m.mode(), 0);
        // During the first tap: it was a Hypershift use, so it arms nothing.
        press(&mut m, 1_000, "KEY_LEFTALT");
        press(&mut m, 1_010, "KEY_1");
        release(&mut m, 1_020, "KEY_1");
        release(&mut m, 1_030, "KEY_LEFTALT");
        quick_tap(&mut m, 1_100, 50);
        assert_eq!(m.mode(), 0);
    }

    #[test]
    fn double_tap_in_a_single_mode_profile_restores_the_led() {
        let cfg = Config {
            hypershift_led_mask: Some(7),
            ..Config::default()
        };
        let mut m = Machine::new(cfg, profile("name='S'\n[mode.0]\n"));
        quick_tap(&mut m, 0, 50);
        let fx = quick_tap(&mut m, 100, 50);
        assert!(
            fx.contains(&Effect::SetLed(1)),
            "LED returns to the mode's colour"
        );
        assert_eq!(m.mode(), 0);
    }

    #[test]
    fn double_tap_shows_the_hypershift_frame_on_press() {
        let mut m = Machine::new(Config::default(), profile(LIT));
        let fx = press(&mut m, 0, "KEY_LEFTALT");
        assert_eq!(w_cell(frames(&fx)[0].as_ref().unwrap()), [16, 16, 16]);
        let back = release(&mut m, 50, "KEY_LEFTALT");
        assert_eq!(w_cell(frames(&back)[0].as_ref().unwrap()), [0, 255, 255]);
    }

    fn select_machine() -> Machine {
        let cfg = Config {
            mode_switch: ModeSwitch::Select,
            select_timeout: ms(2000),
            ..Config::default()
        };
        Machine::new(cfg, profile(PROFILE))
    }

    /// Two quick taps ending at `t + 150 + 50`, which open the selection.
    fn open_select(m: &mut Machine, t: u64) -> Vec<Effect> {
        quick_tap(m, t, 50);
        quick_tap(m, t + 150, 50)
    }

    #[test]
    fn select_double_tap_opens_a_blinking_selection_without_changing_mode() {
        let mut m = select_machine();
        let fx = open_select(&mut m, 0);
        assert_eq!(fx, vec![Effect::ShiftChanged(false), Effect::SetLed(1)]);
        assert!(m.is_selecting());
        assert_eq!(m.mode(), 0);
        // The LED goes dark, then shows the candidate again, until something happens.
        assert_eq!(m.next_deadline(), Some(ms(200 + 300)));
        assert_eq!(m.tick(ms(500)), vec![Effect::SetLed(0)]);
        assert_eq!(m.tick(ms(800)), vec![Effect::SetLed(1)]);
    }

    #[test]
    fn select_dpad_moves_the_candidate_and_a_tap_picks_it() {
        let mut m = select_machine();
        open_select(&mut m, 0);
        assert_eq!(press(&mut m, 300, "KEY_DOWN"), vec![Effect::SetLed(2)]);
        release(&mut m, 310, "KEY_DOWN");
        assert_eq!(press(&mut m, 400, "KEY_DOWN"), vec![Effect::SetLed(4)]);
        // Wraps both ways.
        assert_eq!(press(&mut m, 500, "KEY_DOWN"), vec![Effect::SetLed(1)]);
        assert_eq!(press(&mut m, 600, "KEY_UP"), vec![Effect::SetLed(4)]);
        assert_eq!(m.mode(), 0, "nothing changes until the pick");
        assert_eq!(press(&mut m, 700, "KEY_LEFTALT"), vec![]);
        assert_eq!(
            release(&mut m, 750, "KEY_LEFTALT"),
            vec![Effect::ModeChanged(2), Effect::SetLed(4)]
        );
        assert_eq!(m.mode(), 2);
        assert!(!m.is_selecting());
    }

    #[test]
    fn select_picking_the_current_mode_just_stops_the_blink() {
        let mut m = select_machine();
        open_select(&mut m, 0);
        press(&mut m, 300, "KEY_LEFTALT");
        assert_eq!(release(&mut m, 340, "KEY_LEFTALT"), vec![Effect::SetLed(1)]);
        assert!(!m.is_selecting());
    }

    #[test]
    fn select_times_out_and_restores_the_led() {
        let mut m = select_machine();
        open_select(&mut m, 0); // opened at 200, times out at 2200
        press(&mut m, 300, "KEY_DOWN"); // renews: 2300
        assert_eq!(
            m.tick(ms(2299))
                .iter()
                .filter(|e| matches!(e, Effect::SetLed(_)))
                .count(),
            1
        );
        assert!(m.is_selecting());
        assert_eq!(m.tick(ms(2300)), vec![Effect::SetLed(1)]);
        assert!(!m.is_selecting());
        assert_eq!(m.mode(), 0);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn select_swallows_other_keys_and_a_long_hold_does_not_pick() {
        let mut m = select_machine();
        open_select(&mut m, 0);
        assert_eq!(press(&mut m, 300, "KEY_1"), vec![]);
        assert_eq!(release(&mut m, 310, "KEY_1"), vec![]);
        press(&mut m, 400, "KEY_DOWN");
        press(&mut m, 500, "KEY_LEFTALT");
        assert_eq!(release(&mut m, 500 + 400, "KEY_LEFTALT"), vec![]);
        assert!(m.is_selecting());
        assert_eq!(m.mode(), 0);
    }

    #[test]
    fn select_is_not_offered_in_a_single_mode_profile_and_ends_on_swap() {
        let one = profile("name='S'\n[mode.0]\n");
        let cfg = Config {
            mode_switch: ModeSwitch::Select,
            ..Config::default()
        };
        let mut m = Machine::new(cfg, one);
        open_select(&mut m, 0);
        assert!(!m.is_selecting());

        let mut m = select_machine();
        open_select(&mut m, 0);
        m.swap_profile(profile(PROFILE));
        assert!(!m.is_selecting());
        assert_eq!(press(&mut m, 5_000, "KEY_1"), vec![down("KEY_R")]);
    }

    #[test]
    fn select_leaves_the_other_gestures_alone() {
        // A single tap, a hold and Hypershift use behave as in `double_tap`.
        let mut m = select_machine();
        assert_eq!(quick_tap(&mut m, 0, 50), vec![Effect::ShiftChanged(false)]);
        assert_eq!(m.mode(), 0);
        press(&mut m, 5_000, "KEY_LEFTALT");
        assert_eq!(press(&mut m, 5_010, "KEY_1"), vec![down("KEY_F1")]);
        release(&mut m, 5_020, "KEY_1");
        release(&mut m, 5_030, "KEY_LEFTALT");
        assert!(!m.is_selecting());
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
             KEY_X = { macro = [['KEY_R', 'down', 5], ['KEY_R', 'up', 5]] }\n\
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
            pk("KEY_X"),
            PhysicalKey::Wheel(WheelDir::Up),
            PhysicalKey::Wheel(WheelDir::Down),
        ];
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let cfg = Config {
                hypershift_led_mask: (seed % 2 == 0).then_some(7),
                hypershift_fallthrough: seed % 3 != 0,
                mode_switch: if (seed / 7) % 2 == 0 {
                    ModeSwitch::Tap
                } else {
                    ModeSwitch::DoubleTap
                },
                ..tap_cfg()
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
                } else if r < 8 {
                    let fx = m.tick(ms(now));
                    assert!(
                        m.next_deadline().is_none_or(|d| d > ms(now)),
                        "tick left a due step behind (seed {seed})"
                    );
                    fx
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
            // Let every running macro finish: balanced macros must leave nothing down.
            while let Some(d) = m.next_deadline() {
                now = now.max(d.as_millis() as u64);
                let fx = m.tick(ms(now));
                apply(fx, &mut model);
            }
            assert!(model.is_empty(), "stuck keys {model:?} (seed {seed})");
        }
    }
}
