//! The editor window. State lives in [`State`]; every widget is rebuilt or refreshed from
//! it, so there is exactly one place that says what is selected and what is edited.

mod colors;
mod completion;
mod dialogs;
mod inspector;
mod keypad;
mod picker;
mod preview;
mod selection;
mod settings;
mod sidebar;

use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    path::PathBuf,
    rc::Rc,
};

use adw::prelude::*;
use gtk::{gio, glib};
use tartarusd_core::{PhysicalKey, Profile};
use tartarusd_ipc::DaemonProxy;

use crate::{
    daemon::{self, Status},
    edit::{self, Layer},
    store,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Bindings,
    Lighting,
}

pub struct State {
    pub dir: PathBuf,
    pub entries: Vec<store::Entry>,
    /// File of the open profile, and the copy being edited.
    pub file: Option<String>,
    pub draft: Option<Profile>,
    pub dirty: bool,
    pub mode: usize,
    pub layer: Layer,
    pub view: View,
    /// Keys chosen on the pad, and the one the bindings editor shows.
    pub selection: BTreeSet<PhysicalKey>,
    pub selected: Option<PhysicalKey>,
    pub proxy: Option<DaemonProxy<'static>>,
    pub status: Option<Status>,
    /// Match-rule rows whose title pattern doesn't compile.
    pub bad_rules: Vec<usize>,
}

pub struct Ui {
    pub win: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub st: RefCell<State>,
    pub colors: colors::Colors,
    /// Set while widgets are being filled from state, so their handlers stay quiet.
    pub building: Cell<bool>,
    /// Modifier keys held at the last press on the pad.
    pub mods: Cell<gtk::gdk::ModifierType>,
    pub pump: preview::Pump,
    /// The color picker for the selected keys, so it can follow other edits.
    pub key_picker: RefCell<Option<picker::Picker>>,
    pub w: Widgets,
}

pub struct Widgets {
    pub stack: gtk::Stack,
    pub scroll: gtk::ScrolledWindow,
    pub list: gtk::ListBox,
    pub page: adw::NavigationPage,
    pub save: gtk::Button,
    pub status: gtk::Label,
    pub name: adw::EntryRow,
    pub remember: adw::SwitchRow,
    pub extended: adw::SwitchRow,
    pub rules: gtk::Box,
    pub modes: gtk::Box,
    pub leds: [gtk::CheckButton; 3],
    pub layer: [gtk::ToggleButton; 2],
    pub view: [gtk::ToggleButton; 2],
    pub caps: RefCell<Vec<keypad::Cap>>,
    pub inspector: gtk::Box,
    pub capture: gtk::Button,
    /// The rubber band drawn while dragging a selection box.
    pub band: gtk::Box,
    pub recording: RefCell<Option<inspector::Recording>>,
}

pub const LED_NAMES: [&str; 3] = ["Blue", "Green", "Red"];

pub fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

impl Ui {
    pub fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// Run `f` on the open draft; marks it edited and refreshes what depends on it.
    pub fn edit(self: &Rc<Self>, f: impl FnOnce(&mut Profile)) {
        {
            let mut st = self.st.borrow_mut();
            let Some(d) = st.draft.as_mut() else { return };
            f(d);
            st.dirty = true;
        }
        self.after_edit();
    }

    /// Like [`Ui::edit`], but a failing edit changes nothing (and doesn't dirty the draft);
    /// the reason is shown as a toast.
    pub fn try_edit(
        self: &Rc<Self>,
        f: impl FnOnce(&mut Profile) -> Result<(), edit::EditError>,
    ) -> bool {
        let result = {
            let mut st = self.st.borrow_mut();
            let Some(d) = st.draft.as_mut() else {
                return false;
            };
            let mut copy = d.clone();
            let r = f(&mut copy);
            if r.is_ok() {
                *d = copy;
                st.dirty = true;
            }
            r
        };
        match result {
            Ok(()) => {
                self.after_edit();
                true
            }
            Err(e) => {
                self.toast(&e.to_string());
                false
            }
        }
    }

    pub fn after_edit(self: &Rc<Self>) {
        self.update_header();
        self.refresh_keypad();
    }

    pub fn update_header(&self) {
        let st = self.st.borrow();
        let title = st.draft.as_ref().map_or("tartarusd", |d| d.name.as_str());
        self.w
            .page
            .set_title(&format!("{}{title}", if st.dirty { "• " } else { "" }));
        self.w.save.set_sensitive(st.dirty);
    }

    /// Re-read the directory, keeping the open profile.
    pub fn rescan(self: &Rc<Self>) {
        let dir = self.st.borrow().dir.clone();
        match store::list(&dir) {
            Ok(entries) => self.st.borrow_mut().entries = entries,
            Err(e) => self.toast(&format!("Cannot read {}: {e}", dir.display())),
        }
        self.refresh_list();
    }

    pub fn open_file(self: &Rc<Self>, file: &str) {
        let draft = {
            let st = self.st.borrow();
            st.entries
                .iter()
                .find(|e| e.file == file)
                .map(|e| e.profile.clone())
        };
        match draft {
            Some(Ok(p)) => {
                {
                    let mut st = self.st.borrow_mut();
                    st.file = Some(file.to_owned());
                    st.draft = Some(p);
                    st.dirty = false;
                    st.mode = 0;
                    st.layer = Layer::Base;
                    st.selected = None;
                    st.selection.clear();
                    st.bad_rules.clear();
                }
                self.rebuild_editor();
            }
            Some(Err(e)) => self.toast(&format!("{file} doesn't load: {e}")),
            None => {}
        }
        self.refresh_list();
    }

    pub fn close_profile(self: &Rc<Self>) {
        {
            let mut st = self.st.borrow_mut();
            st.file = None;
            st.draft = None;
            st.dirty = false;
        }
        self.rebuild_editor();
        self.refresh_list();
    }

    /// Fill every widget from the draft.
    pub fn rebuild_editor(self: &Rc<Self>) {
        self.building.set(true);
        {
            let st = self.st.borrow();
            match &st.draft {
                None => self.w.stack.set_visible_child_name("empty"),
                Some(d) => {
                    self.w.stack.set_visible_child_name("editor");
                    self.w.name.set_text(&d.name);
                    self.w.remember.set_active(d.remember_mode);
                    self.w.extended.set_active(d.extended_modes);
                }
            }
        }
        self.building.set(false);
        self.rebuild_rules();
        self.rebuild_modes();
        self.rebuild_inspector();
        self.refresh_keypad();
        self.update_header();
    }

    pub fn rebuild_modes(self: &Rc<Self>) {
        self.building.set(true);
        clear(&self.w.modes);
        let st = self.st.borrow();
        if let Some(d) = &st.draft {
            let mut first: Option<gtk::ToggleButton> = None;
            for (i, m) in d.modes.iter().enumerate() {
                let b = gtk::ToggleButton::new();
                b.set_child(Some(&led_label(&format!("Mode {}", i + 1), m.led_mask)));
                if let Some(f) = &first {
                    b.set_group(Some(f));
                } else {
                    first = Some(b.clone());
                }
                b.set_active(i == st.mode);
                let ui = Rc::clone(self);
                b.connect_toggled(move |b| {
                    if b.is_active() && !ui.building.get() {
                        ui.select_mode(i);
                    }
                });
                self.w.modes.append(&b);
            }
            let mask = d.modes.get(st.mode).map_or(0, |m| m.led_mask);
            for (bit, chk) in self.w.leds.iter().enumerate() {
                chk.set_active(mask & (1 << bit) != 0);
            }
        }
        self.building.set(false);
    }

    pub fn select_mode(self: &Rc<Self>, i: usize) {
        {
            let mut st = self.st.borrow_mut();
            st.mode = i;
        }
        self.building.set(true);
        {
            let st = self.st.borrow();
            let mask = st
                .draft
                .as_ref()
                .and_then(|d| d.modes.get(i))
                .map_or(0, |m| m.led_mask);
            for (bit, chk) in self.w.leds.iter().enumerate() {
                chk.set_active(mask & (1 << bit) != 0);
            }
        }
        self.building.set(false);
        self.rebuild_inspector();
        self.refresh_keypad();
    }

    pub fn add_mode(self: &Rc<Self>) {
        let mut added = None;
        if self.try_edit(|p| edit::add_mode(p).map(|i| added = Some(i))) {
            if let Some(i) = added {
                self.st.borrow_mut().mode = i;
            }
            self.rebuild_modes();
            self.rebuild_inspector();
            self.refresh_keypad();
        }
    }

    pub fn remove_mode(self: &Rc<Self>) {
        let (i, count) = {
            let st = self.st.borrow();
            (st.mode, st.draft.as_ref().map_or(0, |d| d.modes.len()))
        };
        if count <= 1 {
            return self.toast("A profile needs at least one mode");
        }
        let ui = Rc::clone(self);
        self.confirm(
            &format!("Remove mode {}?", i + 1),
            "Deletes its bindings and lighting. Keys that switch to it are cleared.",
            "Remove",
            move || {
                ui.edit(|p| {
                    let _ = edit::remove_mode(p, i);
                });
                ui.st.borrow_mut().mode = i.saturating_sub(1).min(count - 2);
                ui.rebuild_modes();
                ui.rebuild_inspector();
                ui.refresh_keypad();
            },
        );
    }

    pub fn set_led_bit(self: &Rc<Self>, bit: usize, on: bool) {
        let (mode, mask) = {
            let st = self.st.borrow();
            let Some(m) = st.draft.as_ref().and_then(|d| d.modes.get(st.mode)) else {
                return;
            };
            (
                st.mode,
                if on {
                    m.led_mask | (1 << bit)
                } else {
                    m.led_mask & !(1 << bit)
                },
            )
        };
        self.try_edit(|p| edit::set_led_mask(p, mode, mask));
        // Either way, show the truth.
        self.rebuild_modes();
    }

    pub fn save_current(self: &Rc<Self>) -> bool {
        let (dir, file, draft) = {
            let st = self.st.borrow();
            let (Some(f), Some(d)) = (st.file.clone(), st.draft.clone()) else {
                return true;
            };
            if !st.bad_rules.is_empty() {
                drop(st);
                self.toast("Fix the title regex first");
                return false;
            }
            (st.dir.clone(), f, d)
        };
        if store::name_taken(&self.st.borrow().entries, &draft.name, Some(&file)) {
            self.toast("Another profile has that name");
            return false;
        }
        match store::save(&dir, &file, &draft) {
            Ok(()) => {
                self.st.borrow_mut().dirty = false;
                self.toast("Saved");
                self.rescan();
                self.update_header();
                true
            }
            Err(e) => {
                self.toast(&format!("Not saved: {e:#}"));
                false
            }
        }
    }

    pub fn set_status(self: &Rc<Self>, s: Option<Status>) {
        let changed = self.st.borrow().status != s;
        if !changed {
            return;
        }
        let text = match &s {
            Some(s) => format!(
                "● Daemon: {} · mode {}{}",
                s.profile,
                s.mode + 1,
                if s.pinned { " · pinned" } else { "" }
            ),
            None => "○ Daemon not running".to_owned(),
        };
        self.w.status.set_text(&text);
        self.w
            .status
            .set_opacity(if s.is_some() { 1.0 } else { 0.6 });
        self.st.borrow_mut().status = s;
        self.refresh_list();
    }
}

/// `Mode 1 ●●○` with the dots in the LED colors, so the tab shows what the pad will show.
fn led_label(text: &str, mask: u8) -> gtk::Label {
    const COLORS: [&str; 3] = ["#3584e4", "#33d17a", "#e01b24"];
    let mut markup = glib::markup_escape_text(text).to_string();
    markup.push(' ');
    for (bit, color) in COLORS.iter().enumerate() {
        let lit = mask & (1 << bit) != 0;
        markup.push_str(&format!(
            "<span foreground=\"{}\" alpha=\"{}\">●</span>",
            color,
            if lit { "100%" } else { "25%" }
        ));
    }
    let l = gtk::Label::new(None);
    l.set_markup(&markup);
    l
}

fn section(title: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(title));
    l.add_css_class("heading");
    l.set_xalign(0.0);
    l
}

pub fn build(app: &adw::Application, dir: PathBuf) -> Rc<Ui> {
    let w = Widgets {
        stack: gtk::Stack::new(),
        scroll: gtk::ScrolledWindow::builder().vexpand(true).build(),
        list: gtk::ListBox::new(),
        page: adw::NavigationPage::builder().title("tartarusd").build(),
        save: gtk::Button::with_label("Save"),
        status: gtk::Label::new(Some("○ Daemon not running")),
        name: adw::EntryRow::builder().title("Name").build(),
        remember: adw::SwitchRow::builder()
            .title("Remember mode")
            .subtitle("Restore the last mode when this profile is used again")
            .build(),
        extended: adw::SwitchRow::builder()
            .title("Extended modes")
            .subtitle("Allow up to 7 modes (default 3)")
            .build(),
        rules: gtk::Box::new(gtk::Orientation::Vertical, 6),
        modes: gtk::Box::new(gtk::Orientation::Horizontal, 0),
        leds: LED_NAMES.map(gtk::CheckButton::with_label),
        layer: [
            gtk::ToggleButton::with_label("Base"),
            gtk::ToggleButton::with_label("Hypershift"),
        ],
        view: [
            gtk::ToggleButton::with_label("Bindings"),
            gtk::ToggleButton::with_label("Lighting"),
        ],
        caps: RefCell::new(Vec::new()),
        inspector: gtk::Box::new(gtk::Orientation::Vertical, 12),
        capture: gtk::Button::with_label("Identify key on pad"),
        band: gtk::Box::new(gtk::Orientation::Vertical, 0),
        recording: RefCell::new(None),
    };
    let toasts = adw::ToastOverlay::new();
    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("tartarusd")
        .default_width(1180)
        .default_height(860)
        .content(&toasts)
        .build();
    let ui = Rc::new(Ui {
        win,
        toasts,
        st: RefCell::new(State {
            dir,
            entries: Vec::new(),
            file: None,
            draft: None,
            dirty: false,
            mode: 0,
            layer: Layer::Base,
            view: View::Bindings,
            selection: BTreeSet::new(),
            selected: None,
            proxy: None,
            status: None,
            bad_rules: Vec::new(),
        }),
        colors: colors::Colors::install(),
        building: Cell::new(false),
        mods: Cell::new(gtk::gdk::ModifierType::empty()),
        pump: preview::Pump::default(),
        key_picker: RefCell::new(None),
        w,
    });

    let split = adw::NavigationSplitView::new();
    split.set_max_sidebar_width(240.0);
    split.set_sidebar(Some(&sidebar::build(&ui)));
    split.set_content(Some(&content_page(&ui)));
    ui.toasts.set_child(Some(&split));

    install_actions(&ui);
    inspector::install_recorder(&ui);
    selection::install_shortcuts(&ui);
    app.set_accels_for_action("win.save", &["<Primary>s"]);

    let u = Rc::clone(&ui);
    ui.win.connect_close_request(move |win| {
        if !u.st.borrow().dirty {
            if let Some(p) = u.st.borrow().proxy.clone() {
                daemon::stop_capture_now(&p);
            }
            u.end_preview_blocking();
            return glib::Propagation::Proceed;
        }
        let (u2, win) = (Rc::clone(&u), win.clone());
        u.ask_unsaved(move || {
            u2.st.borrow_mut().dirty = false;
            win.close();
        });
        glib::Propagation::Stop
    });
    let u = Rc::clone(&ui);
    ui.win.connect_is_active_notify(move |w| {
        if w.is_active() && !u.st.borrow().dirty {
            u.rescan();
        }
    });

    ui.rescan();
    let first = ui
        .st
        .borrow()
        .entries
        .iter()
        .find(|e| e.profile.is_ok())
        .map(|e| e.file.clone());
    match first {
        Some(f) => ui.open_file(&f),
        None => ui.rebuild_editor(),
    }
    start_daemon_watch(&ui);
    ui
}

fn content_page(ui: &Rc<Ui>) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    ui.w.save.add_css_class("suggested-action");
    ui.w.save.set_sensitive(false);
    ui.w.save.set_action_name(Some("win.save"));
    header.pack_start(&ui.w.save);
    ui.w.status.add_css_class("caption");
    header.set_title_widget(Some(&ui.w.status));

    let menu = gio::Menu::new();
    menu.append(Some("Pin in daemon"), Some("win.pin"));
    menu.append(Some("Unpin"), Some("win.unpin"));
    menu.append(Some("Delete profile…"), Some("win.delete"));
    let general = gio::Menu::new();
    general.append(Some("Settings…"), Some("win.settings"));
    menu.append_section(None, &general);
    let mb = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .build();
    header.pack_end(&mb);

    let col = gtk::Box::new(gtk::Orientation::Vertical, 24);
    col.set_margin_top(18);
    col.set_margin_bottom(24);
    col.set_margin_start(16);
    col.set_margin_end(16);

    col.append(&keypad::keymap_section(ui));

    let group = adw::PreferencesGroup::builder().title("Profile").build();
    group.add(&ui.w.name);
    group.add(&ui.w.remember);
    group.add(&ui.w.extended);
    col.append(&group);
    sidebar::wire_profile_fields(ui);

    if cfg!(feature = "hyprland") {
        col.append(&sidebar::rules_section(ui));
    }

    let clamp = adw::Clamp::builder().maximum_size(980).child(&col).build();
    ui.w.scroll.set_child(Some(&clamp));

    let empty = adw::StatusPage::builder()
        .title("No profile open")
        .description("Select or create a profile.")
        .build();
    ui.w.stack.add_named(&empty, Some("empty"));
    ui.w.stack.add_named(&ui.w.scroll, Some("editor"));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&ui.w.stack));
    ui.w.page.set_child(Some(&view));
    ui.w.page.clone()
}

fn install_actions(ui: &Rc<Ui>) {
    fn add(ui: &Rc<Ui>, name: &str, f: impl Fn(&Rc<Ui>) + 'static) {
        let a = gio::SimpleAction::new(name, None);
        let u = Rc::clone(ui);
        a.connect_activate(move |_, _| f(&u));
        ui.win.add_action(&a);
    }
    add(ui, "save", |u| {
        u.save_current();
    });
    add(ui, "new", Ui::new_profile_dialog);
    add(ui, "import", Ui::import_dialog);
    add(ui, "delete", Ui::delete_dialog);
    add(ui, "pin", |u| u.pin(true));
    add(ui, "unpin", |u| u.pin(false));
    add(ui, "settings", Ui::settings_dialog);
}

impl Ui {
    fn pin(self: &Rc<Self>, on: bool) {
        let Some(proxy) = self.st.borrow().proxy.clone() else {
            return self.toast("Daemon not running");
        };
        let name = self.st.borrow().draft.as_ref().map(|d| d.name.clone());
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            let res = if on {
                match name {
                    Some(n) => proxy
                        .activate_profile(&n)
                        .await
                        .map(|()| format!("Pinned “{n}”")),
                    None => return,
                }
            } else {
                proxy.unpin().await.map(|()| "Unpinned".to_owned())
            };
            match res {
                Ok(m) => ui.toast(&m),
                Err(e) => ui.toast(&format!("Daemon error: {e}")),
            }
            if let Ok(s) = daemon::status(&proxy).await {
                ui.set_status(Some(s));
            }
        });
    }
}

fn start_daemon_watch(ui: &Rc<Ui>) {
    let ui = Rc::clone(ui);
    glib::spawn_future_local(async move {
        loop {
            let proxy = ui.st.borrow().proxy.clone();
            match proxy {
                Some(p) => match daemon::status(&p).await {
                    Ok(s) => ui.set_status(Some(s)),
                    Err(_) => {
                        ui.st.borrow_mut().proxy = None;
                        ui.set_status(None);
                    }
                },
                None => match daemon::connect().await {
                    Ok((p, s)) => {
                        ui.st.borrow_mut().proxy = Some(p);
                        ui.set_status(Some(s));
                        ui.renew_preview();
                    }
                    Err(_) => ui.set_status(None),
                },
            }
            ui.update_capture_sensitivity();
            // The daemon drops a preview after a while; keep it on the pad while wanted.
            ui.renew_preview();
            glib::timeout_future_seconds(2).await;
        }
    });
}
