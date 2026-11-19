//! Active-window sources for automatic profile switching.
//!
//! The daemon core is desktop-independent. Knowing the focused window is
//! compositor-specific, so each backend is an opt-in cargo feature (off by default).
//! A backend is a task that sends an [`ActiveWindow`] whenever focus changes,
//! including once at startup for the window that is already focused.

use tartarusd_core::ActiveWindow;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::info;

#[cfg(feature = "hyprland")]
mod hyprland;

/// Start the first compiled-in backend that detects its compositor. Returns its name
/// and task, or `None` (auto-switching is then off and profiles are chosen by pin or
/// default).
pub fn start(tx: mpsc::Sender<ActiveWindow>) -> Option<(&'static str, JoinHandle<()>)> {
    #[cfg(feature = "hyprland")]
    {
        if let Some(src) = hyprland::detect() {
            return Some(("hyprland", tokio::spawn(src.run(tx))));
        }
        info!("hyprland backend compiled in but Hyprland was not detected");
    }
    #[cfg(not(feature = "hyprland"))]
    info!(
        "built without a window-source backend (e.g. --features hyprland); auto-switching is off"
    );
    drop(tx);
    None
}
