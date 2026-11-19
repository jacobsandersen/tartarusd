//! The profile list, the profile-level fields, and the window-match rules.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use tartarusd_core::{MatchRule, TitleRegex};

use super::{Ui, clear, section};
use crate::edit;

pub fn build(ui: &Rc<Ui>) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    let menu = gio::Menu::new();
    menu.append(Some("New profile…"), Some("win.new"));
    menu.append(Some("Import input-remapper preset…"), Some("win.import"));
    let add = gtk::MenuButton::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("New or imported profile")
        .menu_model(&menu)
        .build();
    header.pack_start(&add);

    ui.w.list.add_css_class("navigation-sidebar");
    ui.w.list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&ui.w.list)
        .vexpand(true)
        .build();

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&scroll));
    adw::NavigationPage::builder()
        .title("Profiles")
        .child(&view)
        .build()
}

impl Ui {
    pub fn refresh_list(self: &Rc<Self>) {
        let l = &self.w.list;
        while let Some(c) = l.first_child() {
            l.remove(&c);
        }
        let st = self.st.borrow();
        for e in &st.entries {
            let row = adw::ActionRow::new();
            row.set_activatable(true);
            let (title, tip) = match &e.profile {
                Ok(p) => (p.name.clone(), None),
                Err(err) => (e.file.clone(), Some(err.clone())),
            };
            row.set_title(&gtk::glib::markup_escape_text(&title));
            row.set_subtitle(&gtk::glib::markup_escape_text(&e.file));
            if let Some(t) = tip {
                row.set_tooltip_text(Some(&t));
                let w = gtk::Image::from_icon_name("dialog-warning-symbolic");
                row.add_suffix(&w);
            }
            if let (Ok(p), Some(s)) = (&e.profile, &st.status)
                && p.name == s.profile
            {
                let dot = gtk::Image::from_icon_name("media-record-symbolic");
                dot.set_tooltip_text(Some("Active in the daemon"));
                dot.add_css_class("success");
                row.add_suffix(&dot);
            }
            if st.file.as_deref() == Some(e.file.as_str()) {
                row.add_css_class("accent");
            }
            let (ui, file) = (Rc::clone(self), e.file.clone());
            row.connect_activated(move |_| {
                if ui.st.borrow().file.as_deref() == Some(file.as_str()) {
                    return;
                }
                let (u, f) = (Rc::clone(&ui), file.clone());
                ui.ask_unsaved(move || u.open_file(&f));
            });
            l.append(&row);
        }
        if st.entries.is_empty() {
            let hint = gtk::Label::new(Some("No profiles yet.\nUse + to create or import one."));
            hint.set_justify(gtk::Justification::Center);
            hint.set_margin_top(24);
            hint.add_css_class("dim-label");
            l.append(&hint);
        }
    }

    pub fn rebuild_rules(self: &Rc<Self>) {
        clear(&self.w.rules);
        let rules: Vec<MatchRule> = self
            .st
            .borrow()
            .draft
            .as_ref()
            .map(|d| d.match_rules.clone())
            .unwrap_or_default();
        for (i, r) in rules.iter().enumerate() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let class = gtk::Entry::builder()
                .placeholder_text("Window class (exact)")
                .text(r.class.clone().unwrap_or_default())
                .hexpand(true)
                .build();
            let title = gtk::Entry::builder()
                .placeholder_text("Title regex (optional)")
                .text(r.title_regex.as_ref().map_or("", |t| t.as_str()))
                .hexpand(true)
                .build();
            let del = gtk::Button::from_icon_name("user-trash-symbolic");
            del.add_css_class("flat");
            del.set_tooltip_text(Some("Remove this rule"));

            let ui = Rc::clone(self);
            class.connect_changed(move |e| {
                if ui.building.get() {
                    return;
                }
                let text = e.text().trim().to_owned();
                ui.edit(|p| {
                    if let Some(r) = p.match_rules.get_mut(i) {
                        r.class = (!text.is_empty()).then_some(text);
                    }
                });
            });
            let ui = Rc::clone(self);
            title.connect_changed(move |e| {
                if ui.building.get() {
                    return;
                }
                let text = e.text().trim().to_owned();
                let compiled = if text.is_empty() {
                    Ok(None)
                } else {
                    TitleRegex::new(&text).map(Some)
                };
                match compiled {
                    Ok(re) => {
                        e.remove_css_class("error");
                        e.set_tooltip_text(None);
                        ui.st.borrow_mut().bad_rules.retain(|b| *b != i);
                        ui.edit(|p| {
                            if let Some(r) = p.match_rules.get_mut(i) {
                                r.title_regex = re;
                            }
                        });
                    }
                    Err(why) => {
                        e.add_css_class("error");
                        e.set_tooltip_text(Some(&why));
                        let mut st = ui.st.borrow_mut();
                        if !st.bad_rules.contains(&i) {
                            st.bad_rules.push(i);
                        }
                    }
                }
            });
            let ui = Rc::clone(self);
            del.connect_clicked(move |_| {
                ui.edit(|p| {
                    if i < p.match_rules.len() {
                        p.match_rules.remove(i);
                    }
                });
                ui.st.borrow_mut().bad_rules.clear();
                ui.rebuild_rules();
            });
            row.append(&class);
            row.append(&title);
            row.append(&del);
            self.w.rules.append(&row);
        }
    }
}

pub fn rules_section(ui: &Rc<Ui>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
    b.append(&section("Automatic switching"));
    let hint = gtk::Label::new(Some(
        "Switch to this profile when the focused window matches. Needs a daemon built with a \
         window backend (for example --features hyprland). Every field you fill in must match.",
    ));
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    hint.add_css_class("dim-label");
    b.append(&hint);
    b.append(&ui.w.rules);
    let add = gtk::Button::with_label("Add rule");
    add.set_halign(gtk::Align::Start);
    let u = Rc::clone(ui);
    add.connect_clicked(move |_| {
        u.edit(|p| p.match_rules.push(MatchRule::default()));
        u.rebuild_rules();
    });
    b.append(&add);
    b
}

pub fn wire_profile_fields(ui: &Rc<Ui>) {
    let u = Rc::clone(ui);
    ui.w.name.connect_changed(move |e| {
        if u.building.get() {
            return;
        }
        let text = e.text().to_string();
        u.edit(|p| p.name = text);
    });
    let u = Rc::clone(ui);
    ui.w.remember.connect_active_notify(move |s| {
        if u.building.get() {
            return;
        }
        let on = s.is_active();
        u.edit(|p| p.remember_mode = on);
    });
    let u = Rc::clone(ui);
    ui.w.extended.connect_active_notify(move |s| {
        if u.building.get() {
            return;
        }
        let on = s.is_active();
        if !u.try_edit(|p| edit::set_extended(p, on)) {
            u.building.set(true);
            s.set_active(!on);
            u.building.set(false);
        }
    });
}
