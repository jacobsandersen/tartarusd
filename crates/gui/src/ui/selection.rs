//! Choosing keys on the pad: click, Ctrl/Shift-click to add or remove, drag a box around
//! several, and keyboard shortcuts for select all, clear, and copy/paste of a color.

use std::{cell::Cell, collections::BTreeSet, rc::Rc};

use adw::prelude::*;
use gtk::{gdk, glib};
use tartarusd_core::PhysicalKey;

use super::{Ui, View};
use crate::layout::SLOTS;

/// Movement (in pixels) before a press turns into a box selection.
const DRAG_THRESHOLD: f64 = 6.0;

impl Ui {
    /// A click on a cap: just this key, or add/remove it with Ctrl or Shift held.
    pub fn click_key(self: &Rc<Self>, key: PhysicalKey) {
        let extend = self
            .mods
            .get()
            .intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK);
        let mut keys = self.st.borrow().selection.clone();
        if extend {
            if !keys.remove(&key) {
                keys.insert(key);
            }
        } else {
            keys = BTreeSet::from([key]);
        }
        self.set_selection(keys, Some(key));
    }

    pub fn select_key(self: &Rc<Self>, key: PhysicalKey) {
        self.set_selection(BTreeSet::from([key]), Some(key));
    }

    pub fn select_all(self: &Rc<Self>) {
        let keys: BTreeSet<_> = SLOTS.iter().map(|s| s.physical()).collect();
        self.set_selection(keys, None);
    }

    pub fn clear_selection(self: &Rc<Self>) {
        self.set_selection(BTreeSet::new(), None);
    }

    /// Change the selection and redraw everything that depends on it.
    pub fn set_selection(
        self: &Rc<Self>,
        keys: BTreeSet<PhysicalKey>,
        primary: Option<PhysicalKey>,
    ) {
        self.set_selection_quiet(keys, primary);
        self.rebuild_inspector();
        self.refresh_keypad();
        self.reveal_inspector();
    }

    /// Like [`Ui::set_selection`] but leaves the inspector alone (used while dragging).
    fn set_selection_quiet(&self, keys: BTreeSet<PhysicalKey>, primary: Option<PhysicalKey>) {
        let mut st = self.st.borrow_mut();
        st.selected = primary
            .filter(|p| keys.contains(p))
            .or_else(|| st.selected.filter(|p| keys.contains(p)))
            .or_else(|| keys.iter().next().copied());
        st.selection = keys;
    }

    /// Whether keyboard shortcuts should leave the key alone: the user is typing.
    fn typing(&self) -> bool {
        gtk::prelude::GtkWindowExt::focus(&self.win).is_some_and(|w| {
            w.dynamic_cast_ref::<gtk::Editable>().is_some() || w.is::<gtk::TextView>()
        })
    }
}

/// Drag a box over the pad to select the caps it touches; hold Ctrl or Shift to add to the
/// current selection instead of replacing it.
pub fn install_gestures(ui: &Rc<Ui>, overlay: &gtk::Overlay) {
    // Remember which modifiers were down at the press, for the cap click handlers.
    let press = gtk::GestureClick::new();
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let u = Rc::clone(ui);
    press.connect_pressed(move |g, _, _, _| u.mods.set(g.current_event_state()));
    overlay.add_controller(press);

    let band = ui.w.band.clone();
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let state = Rc::new((Cell::new(false), Cell::new((0.0, 0.0))));
    let base: Rc<std::cell::RefCell<BTreeSet<PhysicalKey>>> = Rc::default();

    let (s, b, u) = (Rc::clone(&state), Rc::clone(&base), Rc::clone(ui));
    drag.connect_drag_begin(move |g, x, y| {
        s.0.set(false);
        s.1.set((x, y));
        let extend = g
            .current_event_state()
            .intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK);
        *b.borrow_mut() = if extend {
            u.st.borrow().selection.clone()
        } else {
            BTreeSet::new()
        };
    });

    let (s, b, u, band2, ov) = (
        Rc::clone(&state),
        Rc::clone(&base),
        Rc::clone(ui),
        band.clone(),
        overlay.clone(),
    );
    drag.connect_drag_update(move |g, dx, dy| {
        if !s.0.get() {
            if dx.hypot(dy) < DRAG_THRESHOLD {
                return;
            }
            // A real drag: take the gesture away from the cap under the pointer.
            s.0.set(true);
            g.set_state(gtk::EventSequenceState::Claimed);
            band2.set_visible(true);
        }
        let (x0, y0) = s.1.get();
        let (left, top) = (x0.min(x0 + dx), y0.min(y0 + dy));
        let (w, h) = (dx.abs(), dy.abs());
        band2.set_margin_start(left as i32);
        band2.set_margin_top(top as i32);
        band2.set_width_request(w as i32);
        band2.set_height_request(h as i32);

        let mut keys = b.borrow().clone();
        for cap in u.w.caps.borrow().iter() {
            if let Some(r) = cap.btn.compute_bounds(&ov) {
                let (cl, ct) = (f64::from(r.x()), f64::from(r.y()));
                let (cr, cb) = (cl + f64::from(r.width()), ct + f64::from(r.height()));
                if cl < left + w && cr > left && ct < top + h && cb > top {
                    keys.insert(cap.slot.physical());
                }
            }
        }
        u.set_selection_quiet(keys, None);
        u.refresh_keypad();
    });

    let (s, u) = (state, Rc::clone(ui));
    drag.connect_drag_end(move |_, _, _| {
        if s.0.replace(false) {
            band.set_visible(false);
            u.rebuild_inspector();
            u.reveal_inspector();
        }
    });
    overlay.add_controller(drag);
}

/// Window-wide shortcuts. They step aside while a text field has the focus.
pub fn install_shortcuts(ui: &Rc<Ui>) {
    let ctl = gtk::EventControllerKey::new();
    ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
    let u = Rc::clone(ui);
    ctl.connect_key_pressed(move |_, key, _, state| {
        if u.w.recording.borrow().is_some() || u.typing() || u.st.borrow().draft.is_none() {
            return glib::Propagation::Proceed;
        }
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let lighting = u.st.borrow().view == View::Lighting;
        match key {
            gdk::Key::a if ctrl => u.select_all(),
            gdk::Key::Escape => u.clear_selection(),
            gdk::Key::c if ctrl && lighting => u.copy_color(),
            gdk::Key::v if ctrl && lighting => u.paste_color(),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    ui.win.add_controller(ctl);
}
