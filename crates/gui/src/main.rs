//! tartarusd-gui: a profile editor for the Tartarus V2.
//!
//! It edits the profile files the daemon watches, so it works without the daemon; when
//! the daemon is running it adds live status, pinning and "press the key on the pad".

mod complete;
mod daemon;
mod edit;
mod layout;
mod settings;
mod store;
mod text;
mod ui;

use adw::prelude::*;

const APP_ID: &str = "org.tartarusd.Gui";

fn main() -> anyhow::Result<gtk::glib::ExitCode> {
    let dir = store::default_dir()?;
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| {
        gtk::Window::set_default_icon_name("tartarusd");
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        ui::build(app, dir.clone()).win.present();
    });
    Ok(app.run())
}
