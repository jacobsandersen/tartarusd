//! Names and the typed client proxy for the daemon's D-Bus interface
//! (`org.tartarusd.Daemon1`, session bus). The daemon implements the server side.

pub const BUS_NAME: &str = "org.tartarusd.Daemon1";
pub const OBJECT_PATH: &str = "/org/tartarusd/Daemon1";
pub const INTERFACE: &str = "org.tartarusd.Daemon1";

/// How long a capture session lasts without a renewing `StartCapture`. The daemon ends
/// capture on its own so a crashed client can't leave the device dead.
pub const CAPTURE_TIMEOUT_SECS: u64 = 30;

/// How long a previewed frame stays on the pad without a renewing `PreviewFrame`. The daemon
/// clears it on its own so a crashed client can't leave the pad stuck.
pub const PREVIEW_TIMEOUT_SECS: u64 = 20;

#[zbus::proxy(
    interface = "org.tartarusd.Daemon1",
    default_service = "org.tartarusd.Daemon1",
    default_path = "/org/tartarusd/Daemon1"
)]
pub trait Daemon {
    /// Names of all loaded profiles.
    fn list_profiles(&self) -> zbus::Result<Vec<String>>;
    /// Pin a profile by name; auto-switching and the default no longer apply.
    fn activate_profile(&self, name: &str) -> zbus::Result<()>;
    /// Drop the pin and return to the default profile.
    fn unpin(&self) -> zbus::Result<()>;
    /// `(active profile, mode index, pinned)`.
    fn get_state(&self) -> zbus::Result<(String, u8, bool)>;
    fn set_mode(&self, mode: u8) -> zbus::Result<()>;
    /// Stop remapping and report each physical key-down via `KeyCaptured`.
    /// Ends after [`CAPTURE_TIMEOUT_SECS`] unless renewed.
    fn start_capture(&self) -> zbus::Result<()>;
    fn stop_capture(&self) -> zbus::Result<()>;
    /// Show a rendered 84-byte frame on the pad (see `tartarusd_core::lighting`). Ends after
    /// [`PREVIEW_TIMEOUT_SECS`] unless renewed.
    fn preview_frame(&self, frame: &[u8]) -> zbus::Result<()>;
    /// Go back to the profile's own lighting.
    fn clear_preview(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn profile_changed(&self, name: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn mode_changed(&self, mode: u8) -> zbus::Result<()>;
    #[zbus(signal)]
    fn shift_changed(&self, shifted: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    fn key_captured(&self, key: String) -> zbus::Result<()>;
}
