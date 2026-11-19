//! The daemon's mutable state and everything that changes it: input, D-Bus commands,
//! profile reloads. Free of hardware and bus I/O (a [`Sink`] and channels stand in),
//! so it is unit-testable.

use std::{io, path::Path, sync::Arc, time::Duration};

use anyhow::Context;
use evdev::InputEvent;
use tartarusd_core::{ActiveWindow, Effect, Event, Machine, Payload, Profile, ProfileSet};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};
use tracing::{error, info, warn};

use crate::{profiles, translate};

/// Where output effects go (the real uinput devices, or a recorder in tests).
pub trait Sink {
    fn emit(&mut self, effects: &[Effect]) -> io::Result<()>;
}

impl Sink for crate::emit::Outputs {
    fn emit(&mut self, effects: &[Effect]) -> io::Result<()> {
        crate::emit::Outputs::emit(self, effects)
    }
}

/// Notifications for D-Bus subscribers, delivered on a separate task so the input path
/// never waits on the bus.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signal {
    ProfileChanged(String),
    ModeChanged(u8),
    ShiftChanged(bool),
    KeyCaptured(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct State {
    pub profile: String,
    pub mode: u8,
    pub pinned: bool,
}

/// A request from a D-Bus client, answered by the event loop.
pub enum Command {
    ListProfiles(oneshot::Sender<Vec<String>>),
    Activate(String, oneshot::Sender<Result<(), String>>),
    Unpin(oneshot::Sender<()>),
    GetState(oneshot::Sender<State>),
    SetMode(u8, oneshot::Sender<Result<(), String>>),
    StartCapture(oneshot::Sender<()>),
    StopCapture(oneshot::Sender<()>),
}

pub struct App<S: Sink> {
    machine: Machine,
    set: ProfileSet,
    sink: S,
    led_tx: watch::Sender<u8>,
    frame_tx: watch::Sender<Option<Arc<Payload>>>,
    signals: mpsc::UnboundedSender<Signal>,
    /// The profile chosen at startup: the fallback when nothing is pinned or matched.
    default_name: String,
    pinned: Option<String>,
    /// Last window reported by a window source (none until one reports).
    window: Option<ActiveWindow>,
    capture_until: Option<Instant>,
}

impl<S: Sink> App<S> {
    pub fn new(
        machine: Machine,
        set: ProfileSet,
        sink: S,
        led_tx: watch::Sender<u8>,
        frame_tx: watch::Sender<Option<Arc<Payload>>>,
        signals: mpsc::UnboundedSender<Signal>,
    ) -> App<S> {
        let default_name = machine.profile().name.clone();
        App {
            machine,
            set,
            sink,
            led_tx,
            frame_tx,
            signals,
            default_name,
            pinned: None,
            window: None,
            capture_until: None,
        }
    }

    pub fn capture_deadline(&self) -> Option<Instant> {
        self.capture_until
    }

    pub fn is_capturing(&self) -> bool {
        self.capture_until.is_some()
    }

    fn signal(&self, s: Signal) {
        // The receiver only goes away at shutdown.
        let _ = self.signals.send(s);
    }

    /// Perform effects: output first (lowest latency), then LEDs and notifications.
    fn apply(&mut self, fx: Vec<Effect>) -> anyhow::Result<()> {
        self.sink.emit(&fx).context("writing to uinput")?;
        for e in fx {
            match e {
                Effect::SetLed(mask) => {
                    self.led_tx.send_replace(mask);
                }
                Effect::SetFrame(frame) => {
                    self.frame_tx.send_replace(frame);
                }
                Effect::ModeChanged(m) => {
                    info!(mode = m, "mode changed");
                    self.signal(Signal::ModeChanged(m));
                }
                Effect::ShiftChanged(on) => self.signal(Signal::ShiftChanged(on)),
                Effect::Key { .. } | Effect::Wheel(_) => {}
            }
        }
        Ok(())
    }

    /// When the next macro step is due (relative to the daemon's start), if any.
    pub fn macro_deadline(&self) -> Option<Duration> {
        self.machine.next_deadline()
    }

    /// Perform macro steps that are due at `now`.
    pub fn tick(&mut self, now: Duration) -> anyhow::Result<()> {
        let fx = self.machine.tick(now);
        self.apply(fx)
    }

    /// Handle one raw event from a Tartarus node.
    pub fn input(&mut self, now: Duration, raw: &InputEvent) -> anyhow::Result<()> {
        for ev in translate::translate(raw) {
            if self.is_capturing() {
                if let Event::Press(key) = ev {
                    self.signal(Signal::KeyCaptured(key.to_string()));
                }
                continue;
            }
            let fx = self.machine.handle(now, ev);
            self.apply(fx)?;
        }
        Ok(())
    }

    fn swap(&mut self, profile: Arc<Profile>) -> anyhow::Result<()> {
        info!(profile = %profile.name, "switching profile");
        self.signal(Signal::ProfileChanged(profile.name.clone()));
        let fx = self.machine.swap_profile(profile);
        self.apply(fx)
    }

    /// Re-read the profiles directory and swap the active profile if its file changed.
    /// Invalid files keep their previous good version (see `ProfileSet::update`).
    pub fn reload(&mut self, dir: &Path) -> anyhow::Result<()> {
        let files = match profiles::read_files(dir) {
            Ok(f) => f,
            Err(e) => {
                error!("cannot reload profiles from {}: {e}", dir.display());
                return Ok(());
            }
        };
        for e in self.set.update(&files) {
            warn!(file = %e.label, "profile not reloaded: {}", e.error);
        }
        if let Some(name) = &self.pinned
            && self.set.get(name).is_none()
        {
            warn!(profile = %name, "pinned profile is no longer present; falling back");
        }
        self.reevaluate()
    }

    /// Which profile should be active: manual pin > window match > startup default.
    /// `None` only if even the default is gone; the caller then keeps the current one.
    fn select(&self) -> Option<Arc<Profile>> {
        if let Some(p) = self.pinned.as_ref().and_then(|n| self.set.get(n)) {
            return Some(p.clone());
        }
        if let Some(p) = self.window.as_ref().and_then(|w| self.set.auto_match(w)) {
            return Some(p.clone());
        }
        self.set.get(&self.default_name).cloned()
    }

    /// Swap to the selected profile if it differs from the running one. Content is
    /// compared, so unrelated changes never release held keys.
    fn reevaluate(&mut self) -> anyhow::Result<()> {
        match self.select() {
            Some(target) if *target != **self.machine.profile() => self.swap(target),
            Some(_) => Ok(()),
            None => {
                warn!("no profile available to select; keeping the loaded one");
                Ok(())
            }
        }
    }

    /// The focused window changed (from a window-source backend).
    pub fn window_changed(&mut self, w: ActiveWindow) -> anyhow::Result<()> {
        if self.window.as_ref() == Some(&w) {
            return Ok(());
        }
        tracing::debug!(class = %w.class, title = %w.title, "active window");
        self.window = Some(w);
        self.reevaluate()
    }

    pub fn command(&mut self, cmd: Command, now: Instant) -> anyhow::Result<()> {
        // A dropped receiver just means the client went away; ignore send failures.
        match cmd {
            Command::ListProfiles(r) => {
                let _ = r.send(self.set.names());
            }
            Command::GetState(r) => {
                let _ = r.send(State {
                    profile: self.machine.profile().name.clone(),
                    mode: self.machine.mode(),
                    pinned: self.pinned.is_some(),
                });
            }
            Command::Activate(name, r) => {
                let result = match self.set.get(&name) {
                    None => Err(format!("no such profile: {name}")),
                    Some(_) => {
                        self.pinned = Some(name);
                        self.reevaluate()?;
                        Ok(())
                    }
                };
                let _ = r.send(result);
            }
            Command::Unpin(r) => {
                self.pinned = None;
                self.reevaluate()?;
                let _ = r.send(());
            }
            Command::SetMode(n, r) => {
                let modes = self.machine.profile().modes.len();
                let result = if usize::from(n) < modes {
                    let fx = self.machine.set_mode(n);
                    self.apply(fx)?;
                    Ok(())
                } else {
                    Err(format!("mode {n} out of range (profile has {modes} modes)"))
                };
                let _ = r.send(result);
            }
            Command::StartCapture(r) => {
                if !self.is_capturing() {
                    // Don't leave output keys stuck down while remapping is suspended.
                    let fx = self.machine.release_all();
                    self.apply(fx)?;
                    info!("capture started");
                }
                self.capture_until =
                    Some(now + Duration::from_secs(tartarusd_ipc::CAPTURE_TIMEOUT_SECS));
                let _ = r.send(());
            }
            Command::StopCapture(r) => {
                self.stop_capture();
                let _ = r.send(());
            }
        }
        Ok(())
    }

    pub fn stop_capture(&mut self) {
        if self.capture_until.take().is_some() {
            info!("capture stopped");
        }
    }

    /// Release every output key that is down (shutdown).
    pub fn shutdown(&mut self) {
        let fx = self.machine.release_all();
        if let Err(e) = self.sink.emit(&fx) {
            error!("releasing held keys failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use evdev::EventType;
    use tartarusd_core::{Config, Edge, Key};
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::*;

    #[derive(Clone, Default)]
    struct Rec(Rc<RefCell<Vec<Effect>>>);

    impl Sink for Rec {
        fn emit(&mut self, fx: &[Effect]) -> io::Result<()> {
            self.0.borrow_mut().extend(fx.iter().cloned());
            Ok(())
        }
    }

    impl Rec {
        /// Key edges emitted since the last call.
        fn take_keys(&self) -> Vec<(String, Edge)> {
            self.0
                .borrow_mut()
                .drain(..)
                .filter_map(|e| match e {
                    Effect::Key { key, edge } => Some((key.to_string(), edge)),
                    _ => None,
                })
                .collect()
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        app: App<Rec>,
        out: Rec,
        sigs: UnboundedReceiver<Signal>,
        led: watch::Receiver<u8>,
        frame: watch::Receiver<Option<Arc<Payload>>>,
    }

    fn profile_text(name: &str, key1: &str) -> String {
        format!(
            "name = '{name}'\n[mode.0]\nKEY_1 = '{key1}'\n[mode.0.hypershift]\nKEY_1 = 'KEY_F1'\n[mode.1]\n"
        )
    }

    fn fixture(files: &[(&str, String)], start: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        for (n, t) in files {
            std::fs::write(dir.path().join(n), t).unwrap();
        }
        let mut set = ProfileSet::new();
        assert!(
            set.update(&profiles::read_files(dir.path()).unwrap())
                .is_empty()
        );
        let machine = Machine::new(Config::default(), set.pick(Some(start)).unwrap());
        let (led_tx, led) = watch::channel(machine.led());
        let (frame_tx, frame) = watch::channel(machine.current_frame());
        let (sig_tx, sigs) = mpsc::unbounded_channel();
        let out = Rec::default();
        let app = App::new(machine, set, out.clone(), led_tx, frame_tx, sig_tx);
        Fixture {
            dir,
            app,
            out,
            sigs,
            led,
            frame,
        }
    }

    fn one(name: &str) -> Fixture {
        fixture(&[("p.toml", profile_text(name, "KEY_A"))], name)
    }

    fn raw(name: &str, value: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, Key::from_name(name).unwrap().0, value)
    }

    fn cmd<T>(app: &mut App<Rec>, make: impl FnOnce(oneshot::Sender<T>) -> Command) -> T {
        let (tx, mut rx) = oneshot::channel();
        app.command(make(tx), Instant::now()).unwrap();
        rx.try_recv().expect("command answered synchronously")
    }

    fn drain(rx: &mut UnboundedReceiver<Signal>) -> Vec<Signal> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn down(k: &str) -> (String, Edge) {
        (k.into(), Edge::Down)
    }
    fn up(k: &str) -> (String, Edge) {
        (k.into(), Edge::Up)
    }

    #[test]
    fn valid_edit_swaps_releases_held_keys_and_signals() {
        let mut f = one("P");
        f.app.input(Duration::ZERO, &raw("KEY_1", 1)).unwrap();
        assert_eq!(f.out.take_keys(), [down("KEY_A")]);

        std::fs::write(f.dir.path().join("p.toml"), profile_text("P", "KEY_B")).unwrap();
        f.app.reload(f.dir.path()).unwrap();
        assert_eq!(f.out.take_keys(), [up("KEY_A")]);
        assert!(drain(&mut f.sigs).contains(&Signal::ProfileChanged("P".into())));

        f.app.input(Duration::ZERO, &raw("KEY_1", 0)).unwrap();
        f.app.input(Duration::ZERO, &raw("KEY_1", 1)).unwrap();
        assert_eq!(f.out.take_keys(), [down("KEY_B")]);
    }

    #[test]
    fn invalid_unrelated_and_deleted_files_do_not_disturb() {
        let mut f = one("P");
        f.app.input(Duration::ZERO, &raw("KEY_1", 1)).unwrap();
        f.out.take_keys();

        std::fs::write(f.dir.path().join("p.toml"), profile_text("P", "KEY_NOPE")).unwrap();
        f.app.reload(f.dir.path()).unwrap();
        std::fs::write(f.dir.path().join("o.toml"), profile_text("Other", "KEY_C")).unwrap();
        f.app.reload(f.dir.path()).unwrap();
        std::fs::remove_file(f.dir.path().join("p.toml")).unwrap();
        f.app.reload(f.dir.path()).unwrap();

        assert!(
            f.out.take_keys().is_empty(),
            "held key must survive all of that"
        );
        assert!(drain(&mut f.sigs).is_empty());
        assert_eq!(cmd(&mut f.app, Command::GetState).profile, "P");
    }

    fn two() -> Fixture {
        fixture(
            &[
                ("a.toml", profile_text("A", "KEY_A")),
                ("b.toml", profile_text("B", "KEY_B")),
            ],
            "A",
        )
    }

    #[test]
    fn pin_and_unpin() {
        let mut f = two();
        assert_eq!(cmd(&mut f.app, Command::ListProfiles), ["A", "B"]);
        let s = cmd(&mut f.app, Command::GetState);
        assert_eq!((s.profile.as_str(), s.mode, s.pinned), ("A", 0, false));

        assert_eq!(
            cmd(&mut f.app, |r| Command::Activate("B".into(), r)),
            Ok(())
        );
        let s = cmd(&mut f.app, Command::GetState);
        assert_eq!((s.profile.as_str(), s.pinned), ("B", true));
        assert!(drain(&mut f.sigs).contains(&Signal::ProfileChanged("B".into())));

        cmd(&mut f.app, Command::Unpin);
        let s = cmd(&mut f.app, Command::GetState);
        assert_eq!(
            (s.profile.as_str(), s.pinned),
            ("A", false),
            "back to the startup profile"
        );
    }

    #[test]
    fn activating_unknown_profile_fails_without_side_effects() {
        let mut f = two();
        let r = cmd(&mut f.app, |r| Command::Activate("Nope".into(), r));
        assert!(r.unwrap_err().contains("no such profile"));
        let s = cmd(&mut f.app, Command::GetState);
        assert_eq!((s.profile.as_str(), s.pinned), ("A", false));
    }

    #[test]
    fn set_mode_validates_and_signals() {
        let mut f = one("P");
        assert_eq!(cmd(&mut f.app, |r| Command::SetMode(1, r)), Ok(()));
        assert_eq!(*f.led.borrow(), 2, "mode 1 shows green");
        assert!(drain(&mut f.sigs).contains(&Signal::ModeChanged(1)));
        assert!(cmd(&mut f.app, |r| Command::SetMode(5, r)).is_err());
        assert_eq!(cmd(&mut f.app, Command::GetState).mode, 1);
    }

    #[test]
    fn hypershift_emits_shift_signals() {
        let mut f = one("P");
        f.app.input(Duration::ZERO, &raw("KEY_LEFTALT", 1)).unwrap();
        f.app.input(Duration::ZERO, &raw("KEY_1", 1)).unwrap();
        f.app.input(Duration::ZERO, &raw("KEY_1", 0)).unwrap();
        f.app.input(Duration::ZERO, &raw("KEY_LEFTALT", 0)).unwrap();
        assert_eq!(
            drain(&mut f.sigs),
            [Signal::ShiftChanged(true), Signal::ShiftChanged(false)]
        );
        assert_eq!(f.out.take_keys(), [down("KEY_F1"), up("KEY_F1")]);
    }

    #[test]
    fn capture_suspends_remapping_and_reports_presses() {
        let mut f = one("P");
        f.app.input(Duration::ZERO, &raw("KEY_1", 1)).unwrap();
        f.out.take_keys();

        cmd(&mut f.app, Command::StartCapture);
        assert_eq!(
            f.out.take_keys(),
            [up("KEY_A")],
            "held output released on capture start"
        );
        assert!(f.app.capture_deadline().is_some());

        f.app.input(Duration::ZERO, &raw("KEY_1", 0)).unwrap(); // release: ignored
        f.app.input(Duration::ZERO, &raw("KEY_Q", 1)).unwrap();
        f.app.input(Duration::ZERO, &raw("KEY_Q", 0)).unwrap();
        f.app.input(Duration::ZERO, &raw("KEY_LEFTALT", 1)).unwrap();
        assert!(f.out.take_keys().is_empty(), "no output while capturing");
        assert_eq!(
            drain(&mut f.sigs),
            [
                Signal::KeyCaptured("KEY_Q".into()),
                Signal::KeyCaptured("KEY_LEFTALT".into())
            ]
        );

        cmd(&mut f.app, Command::StopCapture);
        assert!(!f.app.is_capturing());
        f.app.input(Duration::ZERO, &raw("KEY_1", 1)).unwrap();
        assert_eq!(f.out.take_keys(), [down("KEY_A")], "remapping resumes");
    }

    fn matching(name: &str, class: &str) -> String {
        format!("name = '{name}'\n[[match]]\nclass = '{class}'\n[mode.0]\n[mode.1]\n")
    }

    fn win(class: &str) -> ActiveWindow {
        ActiveWindow {
            class: class.into(),
            title: String::new(),
        }
    }

    fn game() -> Fixture {
        fixture(
            &[
                ("a.toml", profile_text("Default", "KEY_A")),
                ("g.toml", matching("Game", "game")),
            ],
            "Default",
        )
    }

    fn active(f: &mut Fixture) -> String {
        cmd(&mut f.app, Command::GetState).profile
    }

    #[test]
    fn window_match_switches_and_leaving_returns_to_default() {
        let mut f = game();
        f.app.window_changed(win("game")).unwrap();
        assert_eq!(active(&mut f), "Game");
        assert!(drain(&mut f.sigs).contains(&Signal::ProfileChanged("Game".into())));
        f.app.window_changed(win("browser")).unwrap();
        assert_eq!(active(&mut f), "Default");
        f.app.window_changed(ActiveWindow::default()).unwrap();
        assert_eq!(active(&mut f), "Default", "no focused window -> default");
    }

    #[test]
    fn repeated_identical_window_event_does_nothing() {
        let mut f = game();
        f.app.window_changed(win("game")).unwrap();
        drain(&mut f.sigs);
        f.app.window_changed(win("game")).unwrap();
        assert!(drain(&mut f.sigs).is_empty());
    }

    #[test]
    fn pin_beats_window_match_and_unpin_reverts_to_the_match() {
        let mut f = game();
        f.app.window_changed(win("game")).unwrap();
        assert_eq!(
            cmd(&mut f.app, |r| Command::Activate("Default".into(), r)),
            Ok(())
        );
        assert_eq!(active(&mut f), "Default");
        f.app.window_changed(win("other")).unwrap();
        f.app.window_changed(win("game")).unwrap();
        assert_eq!(
            active(&mut f),
            "Default",
            "pinned: focus changes are ignored"
        );
        cmd(&mut f.app, Command::Unpin);
        assert_eq!(
            active(&mut f),
            "Game",
            "unpin re-evaluates the focused window"
        );
    }

    #[test]
    fn editing_match_rules_re_evaluates_the_focused_window() {
        let mut f = game();
        f.app.window_changed(win("newgame")).unwrap();
        assert_eq!(active(&mut f), "Default");
        std::fs::write(f.dir.path().join("g.toml"), matching("Game", "newgame")).unwrap();
        f.app.reload(f.dir.path()).unwrap();
        assert_eq!(active(&mut f), "Game");
    }

    #[test]
    fn deleting_the_active_profile_falls_back_to_default() {
        let mut f = game();
        f.app.window_changed(win("game")).unwrap();
        std::fs::remove_file(f.dir.path().join("g.toml")).unwrap();
        f.app.reload(f.dir.path()).unwrap();
        assert_eq!(active(&mut f), "Default");
    }

    #[test]
    fn mode_is_remembered_across_window_switches_when_opted_in() {
        let remember =
            "name = 'Game'\nremember_mode = true\n[[match]]\nclass = 'game'\n[mode.0]\n[mode.1]\n";
        let mut f = fixture(
            &[
                ("a.toml", profile_text("Default", "KEY_A")),
                ("g.toml", remember.into()),
            ],
            "Default",
        );
        f.app.window_changed(win("game")).unwrap();
        cmd(&mut f.app, |r| Command::SetMode(1, r)).unwrap();
        f.app.window_changed(win("browser")).unwrap();
        f.app.window_changed(win("game")).unwrap();
        assert_eq!(cmd(&mut f.app, Command::GetState).mode, 1);
    }

    fn lit(name: &str, key: &str, color: &str) -> String {
        format!(
            "name = '{name}'\n[mode.0]\n[mode.0.lighting]\ndefault = '#000000'\n{key} = '{color}'\n[mode.1]\n"
        )
    }

    /// The color of the matrix cell for KEY_1 (row 0, col 0) in a published frame.
    fn first_cell(f: &watch::Receiver<Option<Arc<Payload>>>) -> Option<[u8; 3]> {
        f.borrow().as_ref().map(|p| [p[3], p[4], p[5]])
    }

    #[test]
    fn lighting_follows_mode_and_profile_changes() {
        let mut f = fixture(
            &[
                ("l.toml", lit("Lit", "KEY_1", "#ff0000")),
                ("p.toml", profile_text("Plain", "KEY_A")),
            ],
            "Lit",
        );
        assert_eq!(
            first_cell(&f.frame),
            Some([255, 0, 0]),
            "initial frame published"
        );

        // Mode 1 has no frame of its own: dark, because the profile owns the pad.
        cmd(&mut f.app, |r| Command::SetMode(1, r)).unwrap();
        assert_eq!(first_cell(&f.frame), Some([0, 0, 0]));

        // Switching to a profile without lighting hands the pad back.
        assert_eq!(
            cmd(&mut f.app, |r| Command::Activate("Plain".into(), r)),
            Ok(())
        );
        assert!(f.frame.borrow().is_none());

        // And back again re-publishes (mode resets to 0).
        assert_eq!(
            cmd(&mut f.app, |r| Command::Activate("Lit".into(), r)),
            Ok(())
        );
        assert_eq!(first_cell(&f.frame), Some([255, 0, 0]));
    }

    fn macro_fixture() -> Fixture {
        let text = "name = 'M'\n[mode.0]\nKEY_1 = { macro = [['KEY_E', 0], ['KEY_F', 50]] }\n";
        fixture(&[("m.toml", text.into())], "M")
    }

    #[test]
    fn macros_run_on_ticks_and_report_their_deadline() {
        let mut f = macro_fixture();
        f.app.input(Duration::ZERO, &raw("KEY_1", 1)).unwrap();
        assert_eq!(f.out.take_keys(), [down("KEY_E"), up("KEY_E")]);
        assert_eq!(f.app.macro_deadline(), Some(Duration::from_millis(50)));
        f.app.tick(Duration::from_millis(49)).unwrap();
        assert!(f.out.take_keys().is_empty());
        f.app.tick(Duration::from_millis(50)).unwrap();
        assert_eq!(f.out.take_keys(), [down("KEY_F"), up("KEY_F")]);
        assert_eq!(f.app.macro_deadline(), None);
    }

    #[test]
    fn starting_capture_cancels_a_running_macro() {
        let mut f = macro_fixture();
        f.app.input(Duration::ZERO, &raw("KEY_1", 1)).unwrap();
        f.out.take_keys();
        cmd(&mut f.app, Command::StartCapture);
        assert_eq!(f.app.macro_deadline(), None);
        f.app.tick(Duration::from_secs(5)).unwrap();
        assert!(f.out.take_keys().is_empty(), "no output while capturing");
    }

    #[test]
    fn shutdown_releases_held_keys() {
        let mut f = one("P");
        f.app.input(Duration::ZERO, &raw("KEY_1", 1)).unwrap();
        f.out.take_keys();
        f.app.shutdown();
        assert_eq!(f.out.take_keys(), [up("KEY_A")]);
    }
}
