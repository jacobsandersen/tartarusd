//! The keymap editor: mode tabs, layer/view toggles, and the drawn Tartarus.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tartarusd_core::PhysicalKey;

use super::{LED_NAMES, Ui, View, section};
use crate::{
    daemon,
    edit::{self, Layer},
    layout::{self, SLOTS, Slot},
    text,
};

pub struct Cap {
    pub slot: &'static Slot,
    pub btn: gtk::Button,
    pub bind: gtk::Label,
}

pub fn keymap_section(ui: &Rc<Ui>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 12);
    b.append(&section("Keymap"));

    // Mode tabs, add/remove, LEDs.
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    ui.w.modes.add_css_class("linked");
    bar.append(&ui.w.modes);
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.set_tooltip_text(Some("Add a mode"));
    let u = Rc::clone(ui);
    add.connect_clicked(move |_| u.add_mode());
    let del = gtk::Button::from_icon_name("list-remove-symbolic");
    del.set_tooltip_text(Some("Remove this mode"));
    let u = Rc::clone(ui);
    del.connect_clicked(move |_| u.remove_mode());
    bar.append(&add);
    bar.append(&del);
    let leds = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    leds.append(&gtk::Label::new(Some("Profile LEDs:")));
    for (bit, chk) in ui.w.leds.iter().enumerate() {
        chk.set_tooltip_text(Some(&format!(
            "{} LED while this mode is active",
            LED_NAMES[bit]
        )));
        let u = Rc::clone(ui);
        chk.connect_toggled(move |c| {
            if !u.building.get() {
                u.set_led_bit(bit, c.is_active());
            }
        });
        leds.append(chk);
    }
    b.append(&bar);
    b.append(&leds);

    // Layer and view toggles.
    let toggles = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    let layer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    layer.add_css_class("linked");
    ui.w.layer[1].set_group(Some(&ui.w.layer[0]));
    ui.w.layer[0].set_active(true);
    ui.w.layer[1].set_tooltip_text(Some("The layer that is active while the mode key is held"));
    for (i, t) in ui.w.layer.iter().enumerate() {
        let u = Rc::clone(ui);
        t.connect_toggled(move |t| {
            if t.is_active() && !u.building.get() {
                u.st.borrow_mut().layer = if i == 0 {
                    Layer::Base
                } else {
                    Layer::Hypershift
                };
                u.rebuild_inspector();
                u.refresh_keypad();
            }
        });
        layer.append(t);
    }
    let view = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    view.add_css_class("linked");
    ui.w.view[1].set_group(Some(&ui.w.view[0]));
    ui.w.view[0].set_active(true);
    for (i, t) in ui.w.view.iter().enumerate() {
        let u = Rc::clone(ui);
        t.connect_toggled(move |t| {
            if t.is_active() && !u.building.get() {
                u.st.borrow_mut().view = if i == 0 {
                    View::Bindings
                } else {
                    View::Lighting
                };
                u.rebuild_inspector();
                u.refresh_keypad();
            }
        });
        view.append(t);
    }
    toggles.append(&layer);
    toggles.append(&view);
    ui.w.capture
        .set_tooltip_text(Some("Press a key on the Tartarus to select it here"));
    let u = Rc::clone(ui);
    ui.w.capture.connect_clicked(move |_| u.identify_key());
    toggles.append(&ui.w.capture);
    b.append(&toggles);

    // The pad.
    let grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .halign(gtk::Align::Center)
        .build();
    grid.set_margin_top(8);
    grid.set_margin_bottom(8);
    let mut caps = Vec::new();
    for slot in SLOTS {
        let top = gtk::Label::new(Some(slot.label));
        top.add_css_class("pad-label");
        let bind = gtk::Label::new(None);
        bind.add_css_class("bind-label");
        bind.set_ellipsize(gtk::pango::EllipsizeMode::End);
        bind.set_max_width_chars(7);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 2);
        inner.append(&top);
        inner.append(&bind);
        let btn = gtk::Button::new();
        btn.set_child(Some(&inner));
        btn.add_css_class("keycap");
        let (u, key) = (Rc::clone(ui), slot.physical());
        btn.connect_clicked(move |_| u.select_key(key));
        grid.attach(&btn, slot.col, slot.row, slot.w, slot.h);
        caps.push(Cap { slot, btn, bind });
    }
    // The mode key isn't bindable (it is Hypershift itself), but belongs on the drawing.
    let mode = gtk::Button::with_label("Hyper\nkey");
    mode.set_sensitive(false);
    mode.add_css_class("mode-key");
    mode.set_tooltip_text(Some(
        "The mode key never sends anything: hold it for Hypershift, double-tap it to switch modes",
    ));
    grid.attach(&mode, 4, 3, 1, 1);
    *ui.w.caps.borrow_mut() = caps;
    b.append(&grid);

    ui.w.inspector.set_margin_top(4);
    b.append(&ui.w.inspector);
    b
}

impl Ui {
    pub fn select_key(self: &Rc<Self>, key: PhysicalKey) {
        self.st.borrow_mut().selected = Some(key);
        self.rebuild_inspector();
        self.refresh_keypad();
    }

    /// Redraw every cap from the draft and the selection.
    pub fn refresh_keypad(self: &Rc<Self>) {
        let st = self.st.borrow();
        let caps = self.w.caps.borrow();
        for cap in caps.iter() {
            let key = cap.slot.physical();
            let btn = &cap.btn;
            // Reset to the plain look, then apply what the state asks for.
            for class in ["unbound", "selected", "no-led", "lit"] {
                btn.remove_css_class(class);
            }
            let stale: Vec<String> = btn
                .css_classes()
                .iter()
                .filter(|c| c.starts_with("c-"))
                .map(|c| c.to_string())
                .collect();
            for c in stale {
                btn.remove_css_class(&c);
            }
            if st.selected == Some(key) {
                btn.add_css_class("selected");
            }
            let Some(d) = &st.draft else {
                cap.bind.set_text("");
                continue;
            };
            match st.view {
                View::Bindings => match edit::binding(d, st.mode, st.layer, key) {
                    Some(a) => cap.bind.set_text(&text::describe(a)),
                    None => {
                        cap.bind.set_text("—");
                        btn.add_css_class("unbound");
                    }
                },
                View::Lighting => {
                    cap.bind.set_text("");
                    if !cap.slot.has_led() {
                        btn.add_css_class("no-led");
                        continue;
                    }
                    if let (Some(f), PhysicalKey::Key(k)) = (edit::frame(d, st.mode, st.layer), key)
                    {
                        let c = f.keys.get(&k).copied().unwrap_or(f.default);
                        btn.add_css_class("lit");
                        btn.add_css_class(&self.colors.class(c));
                    }
                }
            }
        }
    }

    pub fn update_capture_sensitivity(&self) {
        let on = self.st.borrow().proxy.is_some();
        self.w.capture.set_sensitive(on);
        self.w.capture.set_tooltip_text(Some(if on {
            "Press a key on the Tartarus to select it here"
        } else {
            "Needs the tartarusd daemon to be running"
        }));
    }

    /// Ask the daemon which pad key is pressed next and select it.
    fn identify_key(self: &Rc<Self>) {
        let Some(proxy) = self.st.borrow().proxy.clone() else {
            return;
        };
        self.w.capture.set_sensitive(false);
        self.w.capture.set_label("Press a key on the pad…");
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            let res = daemon::capture_key(&proxy).await;
            ui.w.capture.set_label("Identify key on pad");
            ui.update_capture_sensitivity();
            match res {
                Ok(key) => match layout::slot_for(key) {
                    Some(_) => ui.select_key(key),
                    None => ui.toast("That's the mode key. It can't be rebound"),
                },
                Err(e) => ui.toast(&e.to_string()),
            }
        });
    }
}
