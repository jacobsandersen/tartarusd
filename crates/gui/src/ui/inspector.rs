//! The panel under the keypad: what the selected key does (bindings view) or what color
//! it is (lighting view).

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tartarusd_core::{Action, Key, PhysicalKey, Rgb};

use super::{Ui, View, clear, colors};
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
    "Not set (unchanged)",
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
        let (sel, view, has_draft) = {
            let st = self.st.borrow();
            (st.selected, st.view, st.draft.is_some())
        };
        if !has_draft {
            return;
        }
        let Some(sel) = sel.filter(|k| layout::slot_for(*k).is_some()) else {
            self.w
                .inspector
                .append(&dim("Select a key on the pad above to edit it."));
            return;
        };
        match view {
            View::Bindings => self.bindings_inspector(sel),
            View::Lighting => self.lighting_inspector(sel),
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
        let slot = layout::slot_for(sel).expect("checked");
        b.append(&dim(&format!(
            "The pad sends {} by default.{}",
            slot.key,
            if layer == Layer::Hypershift {
                " Unset keys here fall through to the base layer."
            } else {
                " Unset keys pass through unchanged."
            }
        )));

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
                let rec = gtk::ToggleButton::with_label("Record");
                rec.set_tooltip_text(Some("Press the keys on your keyboard"));
                row.append(&entry);
                row.append(&rec);
                let err = gtk::Label::new(None);
                err.set_xalign(0.0);
                err.add_css_class("error");
                err.set_visible(false);
                detail.append(&row);
                detail.append(&err);
                detail.append(&dim(
                    "Linux key names joined with +. Several keys are pressed together, like Ctrl+C.",
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
                let (entry2, rec2) = (entry.clone(), rec.clone());
                rec.connect_toggled(move |r| {
                    if r.is_active() {
                        let (e, r2) = (entry2.clone(), rec2.clone());
                        *ui.w.recording.borrow_mut() = Some(Recording {
                            held: Vec::new(),
                            chord: Vec::new(),
                            on_done: Box::new(move |keys| {
                                e.set_text(&text::chord_text(&keys));
                                r2.set_active(false);
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
                    "One step per line. `KEY_E 50` taps E after 50 ms. `KEY_LEFTSHIFT down 0` and \
                     `KEY_LEFTSHIFT up 100` press and release separately. Every key pressed must be \
                     released by the end.",
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

    fn lighting_inspector(self: &Rc<Self>, sel: PhysicalKey) {
        let (mode, layer, frame, has_led) = {
            let st = self.st.borrow();
            let f = st
                .draft
                .as_ref()
                .and_then(|d| edit::frame(d, st.mode, st.layer))
                .cloned();
            (
                st.mode,
                st.layer,
                f,
                layout::slot_for(sel).is_some_and(layout::Slot::has_led),
            )
        };
        let b = &self.w.inspector;
        b.append(&self.heading(sel));

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let own = adw::SwitchRow::builder()
            .title(format!("Custom lighting for the {}", layer_name(layer).to_lowercase()))
            .subtitle(match layer {
                Layer::Base => "Off: this mode doesn't set key colors. If any other mode or layer does, it shows dark.",
                Layer::Hypershift => "Off: holding the mode key keeps this mode's colors.",
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

        if let Some(f) = &frame {
            let default = dialog_button(f.default);
            let row = adw::ActionRow::builder()
                .title("Default color")
                .subtitle("Keys without a color of their own")
                .build();
            row.add_suffix(&default);
            list.append(&row);
            let ui = Rc::clone(self);
            default.connect_rgba_notify(move |b| {
                if !ui.building.get() {
                    let c = colors::from_rgba(&b.rgba());
                    ui.edit(|p| edit::set_default_color(p, mode, layer, c));
                }
            });

            if has_led && let PhysicalKey::Key(key) = sel {
                let current = f.keys.get(&key).copied();
                let btn = dialog_button(current.unwrap_or(f.default));
                let reset = gtk::Button::from_icon_name("edit-clear-symbolic");
                reset.set_valign(gtk::Align::Center);
                reset.add_css_class("flat");
                reset.set_tooltip_text(Some("Use the default color"));
                reset.set_sensitive(current.is_some());
                let row = adw::ActionRow::builder().title("This key").build();
                row.add_suffix(&reset);
                row.add_suffix(&btn);
                list.append(&row);
                let ui = Rc::clone(self);
                btn.connect_rgba_notify(move |b| {
                    if !ui.building.get() {
                        let c = colors::from_rgba(&b.rgba());
                        ui.edit(|p| edit::set_color(p, mode, layer, key, Some(c)));
                        ui.rebuild_inspector();
                    }
                });
                let ui = Rc::clone(self);
                reset.connect_clicked(move |_| {
                    ui.edit(|p| edit::set_color(p, mode, layer, key, None));
                    ui.rebuild_inspector();
                });
            }
        }
        b.append(&list);
        if frame.is_some() && !has_led {
            b.append(&dim(
                "This key has no backlight. Only keys 01–20 and the wheel click do.",
            ));
        }
    }
}

fn dialog_button(c: Rgb) -> gtk::ColorDialogButton {
    let dialog = gtk::ColorDialog::new();
    dialog.set_with_alpha(false);
    let b = gtk::ColorDialogButton::new(Some(dialog));
    b.set_rgba(&colors::to_rgba(c));
    b.set_valign(gtk::Align::Center);
    b
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
