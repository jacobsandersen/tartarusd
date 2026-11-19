//! A suggestion popup for the chord entry: type "shift", press Tab, get `KEY_LEFTSHIFT`.

use std::{cell::RefCell, rc::Rc};

use gtk::{gdk, glib, prelude::*};

use crate::complete::{is_complete, split_last, suggest};

const MAX: usize = 8;

struct Popup {
    popover: gtk::Popover,
    list: gtk::ListBox,
    names: RefCell<Vec<&'static str>>,
}

impl Popup {
    /// Show suggestions for the segment being typed, or hide the popup.
    fn refresh(&self, entry: &gtk::Entry) {
        let text = entry.text();
        let (_, segment) = split_last(&text);
        let found = if segment.is_empty() || is_complete(segment) {
            Vec::new()
        } else {
            suggest(segment, MAX)
        };
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        if found.is_empty() {
            self.names.borrow_mut().clear();
            self.popover.popdown();
            return;
        }
        for (i, n) in found.iter().enumerate() {
            let l = gtk::Label::new(Some(n));
            l.set_xalign(0.0);
            self.list.append(&l);
            // Clicking must not steal focus from the entry, or typing would stop.
            if let Some(row) = self.list.row_at_index(i as i32) {
                row.set_focusable(false);
            }
        }
        if let Some(first) = self.list.row_at_index(0) {
            self.list.select_row(Some(&first));
        }
        *self.names.borrow_mut() = found;
        self.popover.popup();
    }

    fn hide(&self) {
        self.names.borrow_mut().clear();
        self.popover.popdown();
    }

    fn accept(&self, entry: &gtk::Entry, index: usize) {
        let Some(name) = self.names.borrow().get(index).copied() else {
            return;
        };
        let text = entry.text();
        let (settled, _) = split_last(&text);
        let completed = format!("{settled}{name}");
        self.hide();
        entry.set_text(&completed);
        entry.set_position(-1);
    }

    fn visible(&self) -> bool {
        self.popover.is_visible() && !self.names.borrow().is_empty()
    }
}

/// Attach key-name completion to `entry`. Tab or Enter accepts the highlighted suggestion,
/// Up/Down move, Escape closes, and clicking a suggestion accepts it.
pub fn attach(entry: &gtk::Entry) {
    let popover = gtk::Popover::new();
    popover.set_parent(entry);
    popover.set_autohide(false);
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_halign(gtk::Align::Start);
    popover.set_focusable(false);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(true);
    list.set_focusable(false);
    popover.set_child(Some(&list));
    let popup = Rc::new(Popup {
        popover: popover.clone(),
        list: list.clone(),
        names: RefCell::default(),
    });

    // The popover is a child of the entry; detach it before the entry goes away.
    entry.connect_destroy(move |_| popover.unparent());

    let weak = entry.downgrade();
    let p = Rc::clone(&popup);
    entry.connect_changed(move |e| p.refresh(e));

    let p = Rc::clone(&popup);
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| p.hide());
    entry.add_controller(focus);

    let (p, w) = (Rc::clone(&popup), weak.clone());
    list.connect_row_activated(move |_, row| {
        if let Some(e) = w.upgrade() {
            p.accept(&e, usize::try_from(row.index()).unwrap_or(0));
            e.grab_focus_without_selecting();
        }
    });

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let p = popup;
    keys.connect_key_pressed(move |_, key, _, _| {
        if !p.visible() {
            return glib::Propagation::Proceed;
        }
        let current = p.list.selected_row().map_or(0, |r| r.index());
        let last = i32::try_from(p.names.borrow().len()).unwrap_or(1) - 1;
        let select = |i: i32| {
            if let Some(r) = p.list.row_at_index(i.clamp(0, last)) {
                p.list.select_row(Some(&r));
            }
        };
        match key {
            gdk::Key::Down => select(current + 1),
            gdk::Key::Up => select(current - 1),
            gdk::Key::Escape => p.hide(),
            gdk::Key::Tab | gdk::Key::Return | gdk::Key::KP_Enter => {
                if let Some(e) = weak.upgrade() {
                    p.accept(&e, usize::try_from(current).unwrap_or(0));
                }
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    entry.add_controller(keys);
}
