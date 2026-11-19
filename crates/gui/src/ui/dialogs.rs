//! Modal questions: confirmations, name prompts, and the preset import flow.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use tartarusd_core::import::{ImportOptions, import_input_remapper};

use super::Ui;
use crate::store;

impl Ui {
    /// Ask a yes/no question; runs `then` only on confirmation.
    pub fn confirm(&self, heading: &str, body: &str, action: &str, then: impl FnOnce() + 'static) {
        let d = adw::AlertDialog::new(Some(heading), Some(body));
        d.add_response("cancel", "Cancel");
        d.add_response("ok", action);
        d.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
        d.set_close_response("cancel");
        d.choose(Some(&self.win), gio::Cancellable::NONE, move |r| {
            if r == "ok" {
                then();
            }
        });
    }

    /// The open profile has unsaved edits; save, discard or cancel, then run `then`.
    pub fn ask_unsaved(self: &Rc<Self>, then: impl FnOnce() + 'static) {
        if !self.st.borrow().dirty {
            return then();
        }
        let d = adw::AlertDialog::new(
            Some("Save changes?"),
            Some("This profile has unsaved changes."),
        );
        d.add_response("cancel", "Cancel");
        d.add_response("discard", "Discard");
        d.add_response("save", "Save");
        d.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        d.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("save"));
        d.set_close_response("cancel");
        let ui = Rc::clone(self);
        d.choose(Some(&self.win), gio::Cancellable::NONE, move |r| {
            match r.as_str() {
                "save" => {
                    if ui.save_current() {
                        then();
                    }
                }
                "discard" => {
                    ui.st.borrow_mut().dirty = false;
                    then();
                }
                _ => {}
            }
        });
    }

    pub fn new_profile_dialog(self: &Rc<Self>) {
        let ui = Rc::clone(self);
        self.ask_unsaved(move || {
            let entry = adw::EntryRow::builder().title("Profile name").build();
            let d = adw::AlertDialog::new(Some("New profile"), None);
            d.set_extra_child(Some(&entry));
            d.add_response("cancel", "Cancel");
            d.add_response("create", "Create");
            d.set_response_appearance("create", adw::ResponseAppearance::Suggested);
            d.set_default_response(Some("create"));
            d.set_close_response("cancel");
            let u = Rc::clone(&ui);
            d.choose(Some(&ui.win), gio::Cancellable::NONE, move |r| {
                if r == "create" {
                    u.create(&entry.text(), None);
                }
            });
        });
    }

    /// Create and open a new profile file. `from` is a ready-made profile (an import).
    fn create(self: &Rc<Self>, name: &str, from: Option<tartarusd_core::Profile>) {
        let name = name.trim();
        if name.is_empty() {
            return self.toast("A profile needs a name");
        }
        let (dir, taken) = {
            let st = self.st.borrow();
            if store::name_taken(&st.entries, name, None) {
                drop(st);
                return self.toast("Another profile has that name");
            }
            (
                st.dir.clone(),
                st.entries
                    .iter()
                    .map(|e| e.file.clone())
                    .collect::<Vec<_>>(),
            )
        };
        let file = store::file_name_for(name, &taken);
        let mut profile = from.unwrap_or_else(|| store::blank(name));
        profile.name = name.to_owned();
        match store::save(&dir, &file, &profile) {
            Ok(()) => {
                self.rescan();
                self.open_file(&file);
            }
            Err(e) => self.toast(&format!("Could not create: {e:#}")),
        }
    }

    pub fn delete_dialog(self: &Rc<Self>) {
        let (file, name) = {
            let st = self.st.borrow();
            let (Some(f), Some(d)) = (st.file.clone(), st.draft.as_ref()) else {
                return;
            };
            (f, d.name.clone())
        };
        let ui = Rc::clone(self);
        self.confirm(
            &format!("Delete “{name}”?"),
            &format!("This deletes {file}."),
            "Delete",
            move || {
                let dir = ui.st.borrow().dir.clone();
                match store::delete(&dir, &file) {
                    Ok(()) => {
                        ui.close_profile();
                        ui.rescan();
                    }
                    Err(e) => ui.toast(&format!("Could not delete: {e}")),
                }
            },
        );
    }

    pub fn import_dialog(self: &Rc<Self>) {
        let ui = Rc::clone(self);
        self.ask_unsaved(move || {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("input-remapper presets (*.json)"));
            filter.add_suffix("json");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let fd = gtk::FileDialog::builder()
                .title("Import an input-remapper preset")
                .filters(&filters)
                .build();
            let u = Rc::clone(&ui);
            fd.open(Some(&ui.win), gio::Cancellable::NONE, move |res| {
                let Ok(file) = res else { return };
                let Some(path) = file.path() else { return };
                match std::fs::read_to_string(&path) {
                    Ok(json) => {
                        let stem = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        u.import_options_dialog(json, stem);
                    }
                    Err(e) => u.toast(&format!("Cannot read {}: {e}", path.display())),
                }
            });
        });
    }

    fn import_options_dialog(self: &Rc<Self>, json: String, name: String) {
        let entry = adw::EntryRow::builder()
            .title("Profile name")
            .text(name)
            .build();
        let modes = adw::SpinRow::with_range(1.0, 3.0, 1.0);
        modes.set_title("Modes");
        modes.set_subtitle("Mode 1 gets the bindings; other modes are empty");
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.append(&entry);
        list.append(&modes);
        let d = adw::AlertDialog::new(Some("Import preset"), None);
        d.set_extra_child(Some(&list));
        d.add_response("cancel", "Cancel");
        d.add_response("import", "Import");
        d.set_response_appearance("import", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("import"));
        d.set_close_response("cancel");
        let ui = Rc::clone(self);
        d.choose(Some(&self.win), gio::Cancellable::NONE, move |r| {
            if r != "import" {
                return;
            }
            let mut opts = ImportOptions::new(entry.text().trim());
            opts.modes = modes.value() as usize;
            match import_input_remapper(&json, &opts) {
                Ok(done) => {
                    ui.create(&done.profile.name.clone(), Some(done.profile));
                    ui.toast(&format!("Imported {} bindings", done.converted));
                    if !done.warnings.is_empty() {
                        ui.show_warnings(&done.warnings);
                    }
                }
                Err(e) => ui.toast(&format!("Import failed: {e}")),
            }
        });
    }

    fn show_warnings(&self, warnings: &[String]) {
        let d = adw::AlertDialog::new(
            Some("Skipped entries"),
            Some("These could not be converted."),
        );
        let label = gtk::Label::new(Some(&warnings.join("\n")));
        label.set_wrap(true);
        label.set_selectable(true);
        label.set_xalign(0.0);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&label)
            .min_content_height(80)
            .max_content_height(260)
            .propagate_natural_height(true)
            .build();
        d.set_extra_child(Some(&scroll));
        d.add_response("ok", "OK");
        d.present(Some(&self.win));
    }
}
