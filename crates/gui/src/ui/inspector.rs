//! The panel under the keypad: what the selected key does (bindings view) or what color
//! it is (lighting view).

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tartarusd_core::{Action, Key, PhysicalKey, Rgb};

use super::{State, Ui, View, clear, picker};
use crate::{
    edit::{self, Layer},
    layout, text,
};

/// A chord being recorded from the keyboard.
pub struct Recording {
    held: Vec<Key>,
    chord: Vec<Key>,
    on_done: Box<dyn Fn(Vec<Key>)>,
}

const KINDS: [&str; 7] = [
    "Not set",
    "Pass through",
    "Disable",
    "Key or chord",
    "Macro",
    "Switch to mode",
    "Next mode",
];

fn kind_of(a: Option<&Action>) -> u32 {
    match a {
        None => 0,
        Some(Action::Passthrough) => 1,
        Some(Action::Disabled) => 2,
        Some(Action::Chord(_)) => 3,
        Some(Action::Macro(_)) => 4,
        Some(Action::SetMode(_)) => 5,
        Some(Action::NextMode) => 6,
    }
}

fn layer_name(l: Layer) -> &'static str {
    match l {
        Layer::Base => "Base layer",
        Layer::Hypershift => "Hypershift layer",
    }
}

fn dim(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_wrap(true);
    l.set_xalign(0.0);
    l.add_css_class("dim-label");
    l
}

impl Ui {
    pub fn cancel_recording(&self) {
        self.w.recording.borrow_mut().take();
    }

    pub fn rebuild_inspector(self: &Rc<Self>) {
        self.cancel_recording();
        clear(&self.w.inspector);
        let (sel, count, view, has_draft) = {
            let st = self.st.borrow();
            (st.selected, st.selection.len(), st.view, st.draft.is_some())
        };
        if !has_draft {
            return;
        }
        match view {
            View::Lighting => self.lighting_inspector(),
            View::Bindings => match sel.filter(|k| layout::slot_for(*k).is_some()) {
                Some(key) if count == 1 => self.bindings_inspector(key),
                Some(_) => self
                    .w
                    .inspector
                    .append(&dim("Select one key to edit its action.")),
                None => self.w.inspector.append(&dim("Select a key.")),
            },
        }
    }

    fn heading(&self, sel: PhysicalKey) -> gtk::Label {
        let st = self.st.borrow();
        let slot = layout::slot_for(sel).expect("checked by the caller");
        let l = gtk::Label::new(Some(&format!(
            "Key {} · {} · mode {}",
            slot.label,
            layer_name(st.layer),
            st.mode + 1
        )));
        l.add_css_class("title-4");
        l.set_xalign(0.0);
        l
    }

    fn bindings_inspector(self: &Rc<Self>, sel: PhysicalKey) {
        let (mode, layer, cur) = {
            let st = self.st.borrow();
            let cur = st
                .draft
                .as_ref()
                .and_then(|d| edit::binding(d, st.mode, st.layer, sel))
                .cloned();
            (st.mode, st.layer, cur)
        };
        let b = &self.w.inspector;
        b.append(&self.heading(sel));
        if layer == Layer::Hypershift {
            b.append(&dim("Unset keys use the base layer."));
        }

        let drop = gtk::DropDown::from_strings(&KINDS);
        drop.set_selected(kind_of(cur.as_ref()));
        let detail = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&gtk::Label::new(Some("Action")));
        row.append(&drop);
        b.append(&row);
        b.append(&detail);
        self.fill_detail(&detail, sel);

        let (ui, detail) = (Rc::clone(self), detail.clone());
        drop.connect_selected_notify(move |d| {
            if ui.building.get() {
                return;
            }
            ui.cancel_recording();
            let action = match d.selected() {
                1 => Some(Action::Passthrough),
                2 => Some(Action::Disabled),
                5 => Some(Action::SetMode(0)),
                6 => Some(Action::NextMode),
                k @ (3 | 4) => {
                    let st = ui.st.borrow();
                    let keep = st
                        .draft
                        .as_ref()
                        .and_then(|p| edit::binding(p, mode, layer, sel))
                        .cloned();
                    drop_borrow(st);
                    keep.filter(|a| kind_of(Some(a)) == k)
                }
                _ => None,
            };
            ui.edit(|p| edit::set_binding(p, mode, layer, sel, action));
            clear(&detail);
            ui.fill_detail(&detail, sel);
        });
    }

    /// The part of the editor that depends on the kind of action.
    fn fill_detail(self: &Rc<Self>, detail: &gtk::Box, sel: PhysicalKey) {
        let (mode, layer, cur, modes) = {
            let st = self.st.borrow();
            let d = st.draft.as_ref();
            (
                st.mode,
                st.layer,
                d.and_then(|d| edit::binding(d, st.mode, st.layer, sel))
                    .cloned(),
                d.map_or(1, |d| d.modes.len()),
            )
        };
        let kind = match &cur {
            Some(a) => kind_of(Some(a)),
            // Chord and macro kinds have no action until the text is valid.
            None => self
                .w
                .inspector
                .first_child()
                .and_then(|_| find_dropdown(&self.w.inspector))
                .map_or(0, |d| d.selected()),
        };
        match kind {
            3 => {
                let initial = match &cur {
                    Some(Action::Chord(k)) => text::chord_text(k),
                    _ => String::new(),
                };
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let entry = gtk::Entry::builder()
                    .text(initial)
                    .placeholder_text("KEY_LEFTCTRL+KEY_C")
                    .hexpand(true)
                    .build();
                super::completion::attach(&entry);
                let rec = gtk::ToggleButton::with_label("Record");
                rec.set_tooltip_text(Some("Record keys"));
                row.append(&entry);
                row.append(&rec);
                let err = gtk::Label::new(None);
                err.set_xalign(0.0);
                err.add_css_class("error");
                err.set_visible(false);
                detail.append(&row);
                detail.append(&err);
                detail.append(&dim(
                    "Key names joined with +. Type a few letters and press Tab to complete.",
                ));

                let (ui, err2) = (Rc::clone(self), err.clone());
                entry.connect_changed(move |e| {
                    if ui.building.get() {
                        return;
                    }
                    match text::parse_chord(&e.text()) {
                        Ok(a) => {
                            err2.set_visible(false);
                            e.remove_css_class("error");
                            ui.edit(|p| edit::set_binding(p, mode, layer, sel, Some(a)));
                        }
                        Err(why) => {
                            err2.set_text(&why);
                            err2.set_visible(!e.text().is_empty());
                            e.add_css_class("error");
                        }
                    }
                });
                let ui = Rc::clone(self);
                // Weak references: the button's own handler must not keep the button alive.
                let (entry2, rec2) = (entry.downgrade(), rec.downgrade());
                rec.connect_toggled(move |r| {
                    if r.is_active() {
                        let (e, r2) = (entry2.clone(), rec2.clone());
                        *ui.w.recording.borrow_mut() = Some(Recording {
                            held: Vec::new(),
                            chord: Vec::new(),
                            on_done: Box::new(move |keys| {
                                if let Some(e) = e.upgrade() {
                                    e.set_text(&text::chord_text(&keys));
                                }
                                if let Some(r) = r2.upgrade() {
                                    r.set_active(false);
                                }
                            }),
                        });
                        r.set_label("Press the keys…");
                    } else {
                        ui.cancel_recording();
                        r.set_label("Record");
                    }
                });
            }
            4 => {
                let initial = match &cur {
                    Some(Action::Macro(s)) => text::macro_text(s),
                    _ => String::new(),
                };
                let view = gtk::TextView::new();
                view.set_monospace(true);
                view.set_top_margin(6);
                view.set_bottom_margin(6);
                view.set_left_margin(8);
                view.buffer().set_text(&initial);
                let scroll = gtk::ScrolledWindow::builder()
                    .child(&view)
                    .min_content_height(120)
                    .build();
                scroll.add_css_class("card");
                let err = gtk::Label::new(None);
                err.set_xalign(0.0);
                err.set_wrap(true);
                err.add_css_class("error");
                err.set_visible(false);
                detail.append(&scroll);
                detail.append(&err);
                detail.append(&dim(
                    "One step per line: `KEY_E 50` (tap after 50 ms), `KEY_LEFTSHIFT down 0`, \
                     `KEY_LEFTSHIFT up 100`. Every key pressed must be released.",
                ));
                let ui = Rc::clone(self);
                view.buffer().connect_changed(move |buf| {
                    if ui.building.get() {
                        return;
                    }
                    let t = buf.text(&buf.start_iter(), &buf.end_iter(), false);
                    match text::parse_macro(&t) {
                        Ok(a) => {
                            err.set_visible(false);
                            ui.edit(|p| edit::set_binding(p, mode, layer, sel, Some(a)));
                        }
                        Err(why) => {
                            err.set_text(&why);
                            err.set_visible(!t.trim().is_empty());
                        }
                    }
                });
            }
            5 => {
                let target = match cur {
                    Some(Action::SetMode(n)) => f64::from(n) + 1.0,
                    _ => 1.0,
                };
                let spin = adw::SpinRow::with_range(1.0, modes as f64, 1.0);
                spin.set_title("Mode");
                spin.set_value(target);
                let list = gtk::ListBox::new();
                list.add_css_class("boxed-list");
                list.set_selection_mode(gtk::SelectionMode::None);
                list.append(&spin);
                detail.append(&list);
                let ui = Rc::clone(self);
                spin.connect_value_notify(move |s| {
                    if ui.building.get() {
                        return;
                    }
                    let n = (s.value() as u8).saturating_sub(1);
                    ui.edit(|p| edit::set_binding(p, mode, layer, sel, Some(Action::SetMode(n))));
                });
            }
            _ => {}
        }
    }

    fn lighting_inspector(self: &Rc<Self>) {
        let (mode, layer, frame, keys, selected) = {
            let st = self.st.borrow();
            let f = st
                .draft
                .as_ref()
                .and_then(|d| edit::frame(d, st.mode, st.layer))
                .cloned();
            (st.mode, st.layer, f, lit_keys(&st), st.selection.len())
        };
        let b = &self.w.inspector;
        let title = gtk::Label::new(Some(&format!(
            "Lighting · {} · mode {}",
            layer_name(layer),
            mode + 1
        )));
        title.add_css_class("title-4");
        title.set_xalign(0.0);
        b.append(&title);

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let own = adw::SwitchRow::builder()
            .title("Custom lighting")
            .subtitle(match layer {
                Layer::Base => "Off: keys are dark if another mode sets colors.",
                Layer::Hypershift => "Off: uses the mode's colors.",
            })
            .active(frame.is_some())
            .build();
        list.append(&own);
        let ui = Rc::clone(self);
        own.connect_active_notify(move |s| {
            if ui.building.get() {
                return;
            }
            let on = s.is_active();
            ui.edit(|p| edit::set_frame_enabled(p, mode, layer, on));
            ui.rebuild_inspector();
        });
        b.append(&list);

        let Some(frame) = frame else {
            b.append(&dim("Turn on custom lighting to set key colors."));
            return;
        };

        // What is selected, and quick ways to change it.
        let summary = match (selected, keys.len()) {
            (0, _) => {
                "Select keys: click, Ctrl-click, or drag a box. Ctrl+A selects all.".to_owned()
            }
            (_, 0) => "The selected keys have no light.".to_owned(),
            (n, k) if n == k => format!("{k} selected"),
            (n, k) => format!("{k} of {n} selected keys have a light"),
        };
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let label = gtk::Label::new(Some(&summary));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_wrap(true);
        bar.append(&label);
        let all = gtk::Button::with_label("Select all");
        let none = gtk::Button::with_label("Clear");
        let ui = Rc::clone(self);
        all.connect_clicked(move |_| ui.select_all());
        let ui = Rc::clone(self);
        none.connect_clicked(move |_| ui.clear_selection());
        bar.append(&all);
        bar.append(&none);
        b.append(&bar);

        if let Some(first) = keys.first() {
            let initial = {
                let st = self.st.borrow();
                let primary = st.selected.and_then(|p| match p {
                    PhysicalKey::Key(k) if keys.contains(&k) => Some(k),
                    _ => None,
                });
                st.draft
                    .as_ref()
                    .and_then(|d| edit::color_of(d, mode, layer, primary.unwrap_or(*first)))
                    .unwrap_or(frame.default)
            };
            let ui = Rc::clone(self);
            let picker = picker::new(initial, move |c| ui.apply_color(Some(c)));
            b.append(&picker.root);

            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let copy = gtk::Button::with_label("Copy color");
            let paste = gtk::Button::with_label("Paste color");
            let reset = gtk::Button::with_label("Use default");
            let ui = Rc::clone(self);
            copy.connect_clicked(move |_| ui.copy_color());
            let ui = Rc::clone(self);
            paste.connect_clicked(move |_| ui.paste_color());
            let (ui, default) = (Rc::clone(self), frame.default);
            reset.connect_clicked(move |_| {
                ui.apply_color(None);
                picker.set(default);
            });
            for w in [&copy, &paste, &reset] {
                actions.append(w);
            }
            b.append(&actions);
        }

        let expander = gtk::Expander::new(Some("Default color"));
        let ui = Rc::clone(self);
        let default = picker::new(frame.default, move |c| {
            ui.edit(|p| edit::set_default_color(p, mode, layer, c));
        });
        expander.set_child(Some(&default.root));
        b.append(&expander);
    }

    /// Set the color of the selected keys that have a light (`None`: the default color).
    pub fn apply_color(self: &Rc<Self>, c: Option<Rgb>) {
        let (mode, layer, keys) = {
            let st = self.st.borrow();
            (st.mode, st.layer, lit_keys(&st))
        };
        if !keys.is_empty() {
            self.edit(|p| edit::set_colors(p, mode, layer, &keys, c));
        }
    }

    /// The color of the selected key (the one last clicked, if it has a light), if the layer
    /// has lighting.
    fn selected_color(&self) -> Option<Rgb> {
        let st = self.st.borrow();
        let keys = lit_keys(&st);
        let key = st
            .selected
            .and_then(|p| match p {
                PhysicalKey::Key(k) if keys.contains(&k) => Some(k),
                _ => None,
            })
            .or_else(|| keys.first().copied())?;
        edit::color_of(st.draft.as_ref()?, st.mode, st.layer, key)
    }

    pub fn copy_color(self: &Rc<Self>) {
        match self.selected_color() {
            Some(c) => {
                self.win.clipboard().set_text(&c.to_string());
                self.toast(&format!("Copied {c}"));
            }
            None => self.toast("Select a key with a light first"),
        }
    }

    pub fn paste_color(self: &Rc<Self>) {
        let has_frame = {
            let st = self.st.borrow();
            st.draft
                .as_ref()
                .is_some_and(|d| edit::frame(d, st.mode, st.layer).is_some())
        };
        if !has_frame {
            return self.toast("Turn on custom lighting first");
        }
        let ui = Rc::clone(self);
        let clipboard = self.win.clipboard();
        glib::spawn_future_local(async move {
            let text = clipboard.read_text_future().await.ok().flatten();
            match text.as_deref().and_then(picker::parse_hex) {
                Some(c) => {
                    ui.apply_color(Some(c));
                    ui.rebuild_inspector();
                }
                None => ui.toast("The clipboard has no color"),
            }
        });
    }
}

fn find_dropdown(b: &gtk::Box) -> Option<gtk::DropDown> {
    let row = b.first_child()?.next_sibling()?.next_sibling()?;
    let mut c = row.first_child();
    while let Some(w) = c {
        if let Ok(d) = w.clone().downcast::<gtk::DropDown>() {
            return Some(d);
        }
        c = w.next_sibling();
    }
    None
}

fn drop_borrow<T>(_: T) {}

/// Capture-phase key handling so a chord can be recorded without the focused widget
/// reacting to it. GTK's key codes are evdev codes plus 8 on both X11 and Wayland.
pub fn install_recorder(ui: &Rc<Ui>) {
    let ctl = gtk::EventControllerKey::new();
    ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
    let u = Rc::clone(ui);
    ctl.connect_key_pressed(move |_, _, code, _| {
        let mut rec = u.w.recording.borrow_mut();
        let Some(r) = rec.as_mut() else {
            return glib::Propagation::Proceed;
        };
        if let Some(c) = code
            .checked_sub(8)
            .and_then(|c| u16::try_from(c).ok())
            .map(Key)
        {
            if !r.held.contains(&c) {
                r.held.push(c);
            }
            if !r.chord.contains(&c) {
                r.chord.push(c);
            }
        }
        glib::Propagation::Stop
    });
    let u = Rc::clone(ui);
    ctl.connect_key_released(move |_, _, code, _| {
        let done = {
            let mut rec = u.w.recording.borrow_mut();
            let Some(r) = rec.as_mut() else { return };
            if let Some(c) = code
                .checked_sub(8)
                .and_then(|c| u16::try_from(c).ok())
                .map(Key)
            {
                r.held.retain(|k| *k != c);
            }
            if r.held.is_empty() && !r.chord.is_empty() {
                rec.take()
            } else {
                None
            }
        };
        if let Some(r) = done {
            (r.on_done)(r.chord);
        }
    });
    ui.win.add_controller(ctl);
}

/// The selected keys that have a backlight LED.
fn lit_keys(st: &State) -> Vec<Key> {
    st.selection
        .iter()
        .filter_map(|k| match k {
            PhysicalKey::Key(key) if layout::slot_for(*k).is_some_and(layout::Slot::has_led) => {
                Some(*key)
            }
            _ => None,
        })
        .collect()
}
