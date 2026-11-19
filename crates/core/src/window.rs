//! The focused window, as reported by a compositor backend.

/// Both fields are empty when no window has focus.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ActiveWindow {
    /// Window class (X11) / app id (Wayland).
    pub class: String,
    pub title: String,
}
