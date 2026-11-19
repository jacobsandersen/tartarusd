//! The GUI's view of the running daemon. Everything here is optional: the editor works on
//! profile files alone, and these calls only add live status, pinning and key capture.

use std::time::Duration;

use futures_util::StreamExt;
use tartarusd_core::PhysicalKey;
use tartarusd_ipc::DaemonProxy;

/// What `GetState` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub profile: String,
    pub mode: u8,
    pub pinned: bool,
}

/// Connect to the session bus and make sure the daemon answers.
pub async fn connect() -> zbus::Result<(DaemonProxy<'static>, Status)> {
    let conn = zbus::Connection::session().await?;
    let proxy = DaemonProxy::new(&conn).await?;
    let status = status(&proxy).await?;
    Ok((proxy, status))
}

pub async fn status(p: &DaemonProxy<'_>) -> zbus::Result<Status> {
    let (profile, mode, pinned) = p.get_state().await?;
    Ok(Status {
        profile,
        mode,
        pinned,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("the daemon did not answer: {0}")]
    Bus(#[from] zbus::Error),
    #[error("no key was pressed within {0} seconds")]
    TimedOut(u64),
    #[error("the daemon reported an unknown key `{0}`")]
    Unknown(String),
}

/// Ask the daemon which physical key is pressed next. The daemon suspends remapping while
/// it listens, and this always asks it to stop again, even on timeout.
pub async fn capture_key(p: &DaemonProxy<'_>) -> Result<PhysicalKey, CaptureError> {
    // Subscribe first so a very fast key press can't be missed.
    let mut keys = p.receive_key_captured().await?;
    p.start_capture().await?;
    let wait = Duration::from_secs(tartarusd_ipc::CAPTURE_TIMEOUT_SECS);
    let next = async { keys.next().await };
    let timeout = gtk::glib::timeout_future(wait);
    let result = futures_util::future::select(Box::pin(next), Box::pin(timeout)).await;
    let _ = p.stop_capture().await;
    match result {
        futures_util::future::Either::Left((Some(sig), _)) => {
            let name = sig.args()?.key;
            name.parse().map_err(|_| CaptureError::Unknown(name))
        }
        _ => Err(CaptureError::TimedOut(tartarusd_ipc::CAPTURE_TIMEOUT_SECS)),
    }
}

/// Drop the pin if the GUI is closing mid-capture. Best effort.
pub fn stop_capture_now(p: &DaemonProxy<'static>) {
    let p = p.clone();
    gtk::glib::spawn_future_local(async move {
        let _ = p.stop_capture().await;
    });
}
