//! The settings dialog: the global `config.toml` options, with a daemon restart after
//! saving because the daemon reads that file only at startup.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use tartarusd_core::ModeSwitch;

use super::Ui;
use crate::settings::{self, Settings};

const UNIT: &str = "tartarusd.service";

const GESTURES: [(ModeSwitch, &str, &str); 3] = [
    (
        ModeSwitch::DoubleTap,
        "Double tap",
        "Two taps switch to the next mode",
    ),
    (ModeSwitch::Tap, "Tap", "One tap switches to the next mode"),
    (
        ModeSwitch::Select,
        "Select",
        "Two taps open a choice: D-pad up and down pick a mode, one tap confirms",
    ),
];

/// "Blue + Red" for a mask, in the order the LEDs sit on the pad.
fn led_names(mask: u8) -> String {
    ["Blue", "Green", "Red"]
        .iter()
        .enumerate()
        .filter(|(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, n)| *n)
        .collect::<Vec<_>>()
        .join(" + ")
}

struct Form {
    gesture: adw::ComboRow,
    tap: adw::SpinRow,
    timeout: adw::SpinRow,
    fallthrough: adw::SwitchRow,
    leds: adw::ComboRow,
}

impl Form {
    fn new() -> Form {
        let gestures: Vec<&str> = GESTURES.iter().map(|g| g.1).collect();
        let mut leds = vec!["Unchanged".to_owned()];
        leds.extend((1..=7).map(led_names));
        let leds: Vec<&str> = leds.iter().map(String::as_str).collect();
        Form {
            gesture: adw::ComboRow::builder()
                .title("Mode switching")
                .model(&gtk::StringList::new(&gestures))
                .build(),
            tap: adw::SpinRow::builder()
                .title("Tap time")
                .subtitle("Milliseconds. A tap is shorter than this")
                .adjustment(&gtk::Adjustment::new(350.0, 1.0, 5000.0, 10.0, 50.0, 0.0))
                .build(),
            timeout: adw::SpinRow::builder()
                .title("Selection timeout")
                .subtitle("Milliseconds without input before the choice is cancelled")
                .adjustment(&gtk::Adjustment::new(
                    4000.0, 500.0, 60_000.0, 100.0, 1000.0, 0.0,
                ))
                .build(),
            fallthrough: adw::SwitchRow::builder()
                .title("Unbound Hypershift keys")
                .subtitle("Use the key's normal binding")
                .build(),
            leds: adw::ComboRow::builder()
                .title("LEDs while Hypershift is held")
                .model(&gtk::StringList::new(&leds))
                .build(),
        }
    }

    fn fill(&self, s: &Settings) {
        let g = GESTURES
            .iter()
            .position(|g| g.0 == s.mode_switch)
            .unwrap_or(0);
        self.gesture.set_selected(g as u32);
        self.tap.set_value(s.tap_term_ms as f64);
        self.timeout.set_value(s.select_timeout_ms as f64);
        self.fallthrough.set_active(s.hypershift_fallthrough);
        self.leds
            .set_selected(s.hypershift_led_mask.map_or(0, u32::from));
        self.show_gesture();
    }

    fn read(&self) -> Settings {
        Settings {
            mode_switch: GESTURES[(self.gesture.selected() as usize).min(2)].0,
            tap_term_ms: self.tap.value() as u64,
            select_timeout_ms: self.timeout.value() as u64,
            hypershift_fallthrough: self.fallthrough.is_active(),
            hypershift_led_mask: match self.leds.selected() {
                0 => None,
                n => Some(n as u8),
            },
        }
    }

    fn show_gesture(&self) {
        let g = &GESTURES[(self.gesture.selected() as usize).min(2)];
        self.gesture.set_subtitle(g.2);
        self.timeout.set_visible(g.0 == ModeSwitch::Select);
    }
}

impl Ui {
    pub fn settings_dialog(self: &Rc<Self>) {
        let path = match settings::default_path() {
            Ok(p) => p,
            Err(e) => return self.toast(&format!("{e:#}")),
        };
        let loaded = match settings::load(&path) {
            Ok(s) => s,
            Err(e) => return self.toast(&format!("Cannot read settings: {e:#}")),
        };
        let form = Rc::new(Form::new());
        form.fill(&loaded);

        let keys = adw::PreferencesGroup::builder().title("Mode key").build();
        keys.add(&form.gesture);
        keys.add(&form.tap);
        keys.add(&form.timeout);
        let shift = adw::PreferencesGroup::builder().title("Hypershift").build();
        shift.add(&form.fallthrough);
        shift.add(&form.leds);
        let page = adw::PreferencesPage::new();
        page.add(&keys);
        page.add(&shift);

        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        save.set_sensitive(false);
        let header = adw::HeaderBar::new();
        header.pack_end(&save);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&page));
        let dialog = adw::Dialog::builder()
            .title("Settings")
            .content_width(520)
            .child(&view)
            .build();

        let changed = {
            let (form, save) = (Rc::clone(&form), save.clone());
            move || {
                form.show_gesture();
                save.set_sensitive(form.read() != loaded);
            }
        };
        let c = changed.clone();
        form.gesture.connect_selected_notify(move |_| c());
        let c = changed.clone();
        form.tap.connect_value_notify(move |_| c());
        let c = changed.clone();
        form.timeout.connect_value_notify(move |_| c());
        let c = changed.clone();
        form.fallthrough.connect_active_notify(move |_| c());
        form.leds.connect_selected_notify(move |_| changed());

        let (ui, d) = (Rc::clone(self), dialog.clone());
        save.connect_clicked(move |_| {
            if let Err(e) = settings::save(&path, &form.read()) {
                return ui.toast(&format!("Not saved: {e:#}"));
            }
            d.close();
            ui.offer_restart();
        });
        dialog.present(Some(&self.win));
    }

    /// Settings are read at daemon startup, so a running daemon needs a restart.
    fn offer_restart(self: &Rc<Self>) {
        if self.st.borrow().proxy.is_none() {
            return self.toast("Saved");
        }
        let d = adw::AlertDialog::new(
            Some("Restart the daemon?"),
            Some("Saved. The new settings apply after a restart."),
        );
        d.add_response("later", "Later");
        d.add_response("restart", "Restart");
        d.set_response_appearance("restart", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("restart"));
        d.set_close_response("later");
        let ui = Rc::clone(self);
        d.choose(Some(&self.win), gio::Cancellable::NONE, move |r| {
            if r == "restart" {
                ui.restart_daemon();
            }
        });
    }

    fn restart_daemon(self: &Rc<Self>) {
        let ui = Rc::clone(self);
        gtk::glib::spawn_future_local(async move {
            let msg = match run_restart().await {
                Ok(()) => "Daemon restarted".to_owned(),
                Err(e) => format!("Restart failed: {e}"),
            };
            ui.toast(&msg);
        });
    }
}

async fn run_restart() -> Result<(), String> {
    let p = gio::Subprocess::newv(
        &[
            "systemctl".as_ref(),
            "--user".as_ref(),
            "restart".as_ref(),
            UNIT.as_ref(),
        ],
        gio::SubprocessFlags::STDERR_PIPE | gio::SubprocessFlags::STDOUT_SILENCE,
    )
    .map_err(|e| e.to_string())?;
    let (_, err) = p
        .communicate_utf8_future(None)
        .await
        .map_err(|e| e.to_string())?;
    if p.is_successful() {
        Ok(())
    } else {
        let err = err.unwrap_or_default();
        Err(err.lines().next().unwrap_or("systemctl failed").to_owned())
    }
}
