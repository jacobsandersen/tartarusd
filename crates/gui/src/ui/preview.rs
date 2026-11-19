//! Showing the lighting being edited on the real pad, through the daemon's preview call.
//!
//! The editor says what it wants shown; one task at a time sends it, so dragging a slider
//! never queues up calls. The daemon ends a preview by itself after a while, so it is sent
//! again every few seconds while it is wanted.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use gtk::glib;

use super::{Ui, View};
use crate::edit;

#[derive(Default)]
pub struct Pump {
    /// The frame the editor wants on the pad; `None` means the profile's own lighting.
    want: RefCell<Option<Vec<u8>>>,
    /// What the daemon was last told.
    sent: RefCell<Option<Vec<u8>>>,
    busy: Cell<bool>,
}

impl Ui {
    /// Work out what should be previewed and send it if it changed.
    pub fn sync_preview(self: &Rc<Self>) {
        let want = {
            let st = self.st.borrow();
            match (&st.draft, st.view) {
                (Some(d), View::Lighting) => edit::preview_payload(d, st.mode, st.layer),
                _ => None,
            }
        };
        *self.pump.want.borrow_mut() = want;
        self.run_pump();
    }

    /// Send the wanted frame again (the daemon was restarted, or its timeout is near).
    pub fn renew_preview(self: &Rc<Self>) {
        *self.pump.sent.borrow_mut() = None;
        self.run_pump();
    }

    fn run_pump(self: &Rc<Self>) {
        if self.pump.busy.replace(true) {
            return;
        }
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            loop {
                let want = ui.pump.want.borrow().clone();
                if want == *ui.pump.sent.borrow() {
                    break;
                }
                let Some(proxy) = ui.st.borrow().proxy.clone() else {
                    break;
                };
                let res = match &want {
                    Some(frame) => proxy.preview_frame(frame).await,
                    None => proxy.clear_preview().await,
                };
                if res.is_err() {
                    break;
                }
                *ui.pump.sent.borrow_mut() = want;
            }
            ui.pump.busy.set(false);
        });
    }

    /// Give the pad back before the window closes. Waits briefly for the daemon.
    pub fn end_preview_blocking(self: &Rc<Self>) {
        *self.pump.want.borrow_mut() = None;
        if self.pump.sent.borrow().is_none() {
            return;
        }
        self.run_pump();
        let ctx = glib::MainContext::default();
        let start = Instant::now();
        while self.pump.busy.get() && start.elapsed() < Duration::from_millis(400) {
            ctx.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
