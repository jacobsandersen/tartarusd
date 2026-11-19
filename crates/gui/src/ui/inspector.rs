//! The panel under the keypad: what the selected key does (bindings view) or what color
//! it is (lighting view).

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tartarusd_core::{
    Action, Key, PadEffect, PhysicalKey, Rgb, Travel,
    lighting::{REACTIVE_MAX_SPEED, STARLIGHT_MAX_SPEED},
};

use super::{State, Ui, View, clear, picker};
use crate::{
    edit::{self, Layer, LightKind},
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
        self.key_picker.borrow_mut().take();
        clear(&self.w.inspector);
        let (keys, view, has_draft) = {
            let st = self.st.borrow();
            let keys: Vec<PhysicalKey> = st
                .selection
                .iter()
                .copied()
                .filter(|k| layout::slot_for(*k).is_some())
                .collect();
            (keys, st.view, st.draft.is_some())
        };
        if !has_draft {
            return;
        }
        match view {
            View::Lighting => self.lighting_inspector(),
            View::Bindings if keys.is_empty() => {
                self.w.inspector.append(&dim("Select a key."));
            }
            View::Bindings => self.bindings_inspector(keys),
        }
    }

    fn heading(&self, keys: &[PhysicalKey]) -> gtk::Label {
        let st = self.st.borrow();
        let what = match keys {
            [one] => format!("Key {}", layout::slot_for(*one).map_or("?", |s| s.label)),
            many => format!("{} keys", many.len()),
        };
        let l = gtk::Label::new(Some(&format!(
            "{what} · {} · mode {}",
            layer_name(st.layer),
            st.mode + 1
        )));
        l.add_css_class("title-4");
        l.set_xalign(0.0);
        l
    }

    fn bindings_inspector(self: &Rc<Self>, keys: Vec<PhysicalKey>) {
        let keys: Rc<[PhysicalKey]> = keys.into();
        let (mode, layer, common) = {
            let st = self.st.borrow();
            let common = st
                .draft
                .as_ref()
                .and_then(|d| edit::common_binding(d, st.mode, st.layer, &keys));
            (st.mode, st.layer, common)
        };
        // `None`: the keys have different actions. The menu can't be left empty, so it gets
        // an extra first entry saying so.
        let mixed = common.is_none();
        let offset = u32::from(mixed);
        let kind = common.as_ref().map_or(0, |a| kind_of(a.as_ref()));
        let b = &self.w.inspector;
        b.append(&self.heading(&keys));
        if layer == Layer::Hypershift {
            b.append(&dim("Unset keys use the base layer."));
        }
        if mixed {
            b.append(&dim("Choose an action to apply to all of these keys."));
        }

        let mut names: Vec<&str> = Vec::new();
        if mixed {
            names.push("Different actions");
        }
        names.extend(KINDS);
        let drop = gtk::DropDown::from_strings(&names);
        drop.set_selected(if mixed { 0 } else { kind });
        let detail = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&gtk::Label::new(Some("Action")));
        row.append(&drop);
        b.append(&row);
        b.append(&detail);
        self.fill_detail(&detail, &keys, if mixed { u32::MAX } else { kind });

        let (ui, detail) = (Rc::clone(self), detail.clone());
        drop.connect_selected_notify(move |d| {
            // The extra first entry of a mixed selection is not an action.
            let Some(kind) = d.selected().checked_sub(offset) else {
                return;
            };
            if ui.building.get() {
                return;
            }
            ui.cancel_recording();
            // `None`: change nothing yet. A key or macro needs input first, and until it is
            // valid the keys keep what they have.
            let action = match kind {
                0 => Some(None),
                1 => Some(Some(Action::Passthrough)),
                2 => Some(Some(Action::Disabled)),
                5 => Some(Some(Action::SetMode(0))),
                6 => Some(Some(Action::NextMode)),
                _ => None,
            };
            if let Some(action) = action {
                ui.edit(|p| edit::set_bindings(p, mode, layer, &keys, action));
            }
            clear(&detail);
            ui.fill_detail(&detail, &keys, kind);
        });
    }

    /// The part of the editor that depends on the kind of action (`kind` is the menu entry).
    fn fill_detail(self: &Rc<Self>, detail: &gtk::Box, keys: &Rc<[PhysicalKey]>, kind: u32) {
        let (mode, layer, cur, modes) = {
            let st = self.st.borrow();
            let d = st.draft.as_ref();
            (
                st.mode,
                st.layer,
                d.and_then(|d| edit::common_binding(d, st.mode, st.layer, keys))
                    .flatten(),
                d.map_or(1, |d| d.modes.len()),
            )
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

                let (ui, err2, keys) = (Rc::clone(self), err.clone(), Rc::clone(keys));
                entry.connect_changed(move |e| {
                    if ui.building.get() {
                        return;
                    }
                    match text::parse_chord(&e.text()) {
                        Ok(a) => {
                            err2.set_visible(false);
                            e.remove_css_class("error");
                            ui.edit(|p| edit::set_bindings(p, mode, layer, &keys, Some(a)));
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
                let (ui, keys) = (Rc::clone(self), Rc::clone(keys));
                view.buffer().connect_changed(move |buf| {
                    if ui.building.get() {
                        return;
                    }
                    let t = buf.text(&buf.start_iter(), &buf.end_iter(), false);
                    match text::parse_macro(&t) {
                        Ok(a) => {
                            err.set_visible(false);
                            ui.edit(|p| edit::set_bindings(p, mode, layer, &keys, Some(a)));
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
                let (ui, keys) = (Rc::clone(self), Rc::clone(keys));
                spin.connect_value_notify(move |s| {
                    if ui.building.get() {
                        return;
                    }
                    let n = (s.value() as u8).saturating_sub(1);
                    ui.edit(|p| {
                        edit::set_bindings(p, mode, layer, &keys, Some(Action::SetMode(n)));
                    });
                });
            }
            _ => {}
        }
    }

    fn lighting_inspector(self: &Rc<Self>) {
        let (mode, layer, kind, frame, effect, keys, selected) = {
            let st = self.st.borrow();
            let d = st.draft.as_ref();
            (
                st.mode,
                st.layer,
                d.map_or(LightKind::Off, |d| edit::kind(d, st.mode, st.layer)),
                d.and_then(|d| edit::frame(d, st.mode, st.layer)).cloned(),
                d.and_then(|d| edit::effect(d, st.mode, st.layer)).cloned(),
                lit_keys(&st),
                st.selection.len(),
            )
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
        let kinds = adw::ComboRow::builder()
            .title("Lighting")
            .subtitle(match layer {
                Layer::Base => "Off: keys are dark if another mode sets lighting.",
                Layer::Hypershift => "Off: uses the mode's lighting.",
            })
            .model(&gtk::StringList::new(&[
                "Off",
                "Key colors",
                "Firmware effect",
            ]))
            .selected(match kind {
                LightKind::Off => 0,
                LightKind::Keys => 1,
                LightKind::Effect => 2,
            })
            .build();
        list.append(&kinds);
        let ui = Rc::clone(self);
        kinds.connect_selected_notify(move |r| {
            if ui.building.get() {
                return;
            }
            let to = match r.selected() {
                0 => LightKind::Off,
                1 => LightKind::Keys,
                _ => LightKind::Effect,
            };
            ui.edit(|p| edit::set_kind(p, mode, layer, to));
            ui.rebuild_inspector();
        });
        b.append(&list);

        if layer == Layer::Hypershift {
            self.copy_from_base(mode);
        }

        if let Some(effect) = effect {
            self.effect_controls(mode, layer, &effect);
            return;
        }
        let Some(frame) = frame else {
            b.append(&dim("Choose key colors or a firmware effect."));
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
            *self.key_picker.borrow_mut() = Some(picker.clone());

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
        expander.set_expanded(keys.is_empty());
        let ui = Rc::clone(self);
        let default = picker::new(frame.default, move |c| {
            ui.edit(|p| edit::set_default_color(p, mode, layer, c));
            // Keys that use the default changed color; keep the selected-keys picker honest.
            ui.sync_key_picker();
        });
        expander.set_child(Some(&default.root));
        b.append(&expander);

        let tools = gtk::ListBox::new();
        tools.add_css_class("boxed-list");
        tools.set_selection_mode(gtk::SelectionMode::None);
        let pct = adw::SpinRow::with_range(10.0, 200.0, 10.0);
        pct.set_title("Brightness");
        pct.set_subtitle("Scale every color on this layer, in percent");
        pct.set_value(100.0);
        let apply = gtk::Button::with_label("Apply");
        apply.set_valign(gtk::Align::Center);
        pct.add_suffix(&apply);
        tools.append(&pct);
        let ui = Rc::clone(self);
        apply.connect_clicked(move |_| {
            let v = pct.value() as u32;
            ui.edit(|p| edit::scale_layer(p, mode, layer, v));
            ui.rebuild_inspector();
        });
        b.append(&tools);
    }

    /// Controls for a firmware effect: its type, direction, colors and speed.
    fn effect_controls(self: &Rc<Self>, mode: usize, layer: Layer, effect: &PadEffect) {
        let b = &self.w.inspector;
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);

        let ty = adw::ComboRow::builder()
            .title("Effect")
            .model(&gtk::StringList::new(&edit::EFFECT_TYPES))
            .selected(edit::effect_type_index(effect))
            .build();
        let ui = Rc::clone(self);
        ty.connect_selected_notify(move |r| {
            if ui.building.get() {
                return;
            }
            let index = r.selected();
            ui.edit(|p| {
                edit::update_effect(p, mode, layer, |e| *e = edit::with_effect_type(e, index));
            });
            ui.rebuild_inspector();
        });
        list.append(&ty);

        if let PadEffect::Wave(d) = effect {
            let dir = adw::ComboRow::builder()
                .title("Direction")
                .model(&gtk::StringList::new(&["Right to left", "Left to right"]))
                .selected(u32::from(*d == Travel::Right))
                .build();
            let ui = Rc::clone(self);
            dir.connect_selected_notify(move |r| {
                if !ui.building.get() {
                    let to = if r.selected() == 0 {
                        Travel::Left
                    } else {
                        Travel::Right
                    };
                    ui.edit(|p| edit::update_effect(p, mode, layer, |e| *e = PadEffect::Wave(to)));
                }
            });
            list.append(&dir);
        }

        let colors: Vec<Rgb> = match effect {
            PadEffect::Breathing(c) | PadEffect::Starlight { colors: c, .. } => c.clone(),
            PadEffect::Reactive { color, .. } => vec![*color],
            _ => Vec::new(),
        };
        if matches!(
            effect,
            PadEffect::Breathing(_) | PadEffect::Starlight { .. }
        ) {
            let count = adw::ComboRow::builder()
                .title("Colors")
                .model(&gtk::StringList::new(&[
                    "Random",
                    "One color",
                    "Two colors",
                ]))
                .selected(colors.len().min(2) as u32)
                .build();
            let ui = Rc::clone(self);
            count.connect_selected_notify(move |r| {
                if ui.building.get() {
                    return;
                }
                let n = r.selected() as usize;
                ui.edit(|p| edit::update_effect(p, mode, layer, |e| edit::set_color_count(e, n)));
                ui.rebuild_inspector();
            });
            list.append(&count);
        }

        let max_speed = match effect {
            PadEffect::Reactive { .. } => Some((REACTIVE_MAX_SPEED, effect_speed(effect))),
            PadEffect::Starlight { .. } => Some((STARLIGHT_MAX_SPEED, effect_speed(effect))),
            _ => None,
        };
        if let Some((max, current)) = max_speed {
            let speed = adw::SpinRow::with_range(1.0, f64::from(max), 1.0);
            speed.set_title("Speed");
            speed.set_subtitle(&format!("1 is fastest, {max} is slowest"));
            speed.set_value(f64::from(current));
            let ui = Rc::clone(self);
            speed.connect_value_notify(move |s| {
                if ui.building.get() {
                    return;
                }
                let v = s.value() as u8;
                ui.edit(|p| {
                    edit::update_effect(p, mode, layer, |e| match e {
                        PadEffect::Reactive { speed, .. } | PadEffect::Starlight { speed, .. } => {
                            *speed = v;
                        }
                        _ => {}
                    });
                });
            });
            list.append(&speed);
        }
        b.append(&list);

        for (i, c) in colors.into_iter().enumerate() {
            let expander = gtk::Expander::new(Some(&format!("Color {}", i + 1)));
            expander.set_expanded(true);
            let ui = Rc::clone(self);
            let picker = picker::new(c, move |new| {
                ui.edit(|p| {
                    edit::update_effect(p, mode, layer, |e| match e {
                        PadEffect::Breathing(cs) | PadEffect::Starlight { colors: cs, .. } => {
                            if let Some(slot) = cs.get_mut(i) {
                                *slot = new;
                            }
                        }
                        PadEffect::Reactive { color, .. } if i == 0 => *color = new,
                        _ => {}
                    });
                });
            });
            expander.set_child(Some(&picker.root));
            b.append(&expander);
        }
        b.append(&dim(
            "The pad runs this effect itself; there are no per-key colors.",
        ));
    }

    /// Show the selected keys' current color in their picker.
    fn sync_key_picker(&self) {
        if let (Some(p), Some(c)) = (self.key_picker.borrow().as_ref(), self.selected_color()) {
            p.set(c);
        }
    }

    /// "Start from the base layer": copy its lighting here, dimmed, as a starting point.
    fn copy_from_base(self: &Rc<Self>, mode: usize) {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let pct = adw::SpinRow::with_range(5.0, 100.0, 5.0);
        pct.set_title("Copy base layer lighting");
        pct.set_subtitle("Replaces this layer's lighting. Brightness in percent");
        pct.set_value(40.0);
        let copy = gtk::Button::with_label("Copy");
        copy.set_valign(gtk::Align::Center);
        pct.add_suffix(&copy);
        list.append(&pct);
        let ui = Rc::clone(self);
        copy.connect_clicked(move |_| {
            let v = pct.value() as u32;
            let done = ui.try_edit(|p| {
                if edit::copy_layer(p, mode, Layer::Base, Layer::Hypershift, v) {
                    Ok(())
                } else {
                    Err(edit::EditError::NothingToCopy)
                }
            });
            if done {
                ui.rebuild_inspector();
            }
        });
        self.w.inspector.append(&list);
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

fn effect_speed(e: &PadEffect) -> u8 {
    match e {
        PadEffect::Reactive { speed, .. } | PadEffect::Starlight { speed, .. } => *speed,
        _ => 2,
    }
}
