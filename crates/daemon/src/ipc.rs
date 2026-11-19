//! D-Bus server for `org.tartarusd.Daemon1` (session bus). Method calls become
//! [`Command`]s answered by the event loop; [`Signal`]s flow out on their own task so
//! the input path never waits on the bus.

use tartarusd_ipc::{BUS_NAME, OBJECT_PATH};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;
use zbus::{Connection, fdo, object_server::SignalEmitter};

use crate::app::{Command, Signal};

pub struct Service {
    cmds: mpsc::Sender<Command>,
}

async fn ask<T>(
    tx: &mpsc::Sender<Command>,
    make: impl FnOnce(oneshot::Sender<T>) -> Command,
) -> fdo::Result<T> {
    let (reply, answer) = oneshot::channel();
    tx.send(make(reply))
        .await
        .map_err(|_| fdo::Error::Failed("daemon is shutting down".into()))?;
    answer
        .await
        .map_err(|_| fdo::Error::Failed("daemon is shutting down".into()))
}

fn invalid(r: Result<(), String>) -> fdo::Result<()> {
    r.map_err(fdo::Error::InvalidArgs)
}

#[zbus::interface(name = "org.tartarusd.Daemon1")]
impl Service {
    async fn list_profiles(&self) -> fdo::Result<Vec<String>> {
        ask(&self.cmds, Command::ListProfiles).await
    }

    async fn activate_profile(&self, name: String) -> fdo::Result<()> {
        invalid(ask(&self.cmds, |r| Command::Activate(name, r)).await?)
    }

    async fn unpin(&self) -> fdo::Result<()> {
        ask(&self.cmds, Command::Unpin).await
    }

    async fn get_state(&self) -> fdo::Result<(String, u8, bool)> {
        let s = ask(&self.cmds, Command::GetState).await?;
        Ok((s.profile, s.mode, s.pinned))
    }

    async fn set_mode(&self, mode: u8) -> fdo::Result<()> {
        invalid(ask(&self.cmds, |r| Command::SetMode(mode, r)).await?)
    }

    async fn start_capture(&self) -> fdo::Result<()> {
        ask(&self.cmds, Command::StartCapture).await
    }

    async fn stop_capture(&self) -> fdo::Result<()> {
        ask(&self.cmds, Command::StopCapture).await
    }

    async fn preview_frame(&self, frame: Vec<u8>) -> fdo::Result<()> {
        invalid(ask(&self.cmds, |r| Command::PreviewFrame(frame, r)).await?)
    }

    async fn clear_preview(&self) -> fdo::Result<()> {
        ask(&self.cmds, Command::ClearPreview).await
    }

    #[zbus(signal)]
    async fn profile_changed(emitter: &SignalEmitter<'_>, name: String) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn mode_changed(emitter: &SignalEmitter<'_>, mode: u8) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn shift_changed(emitter: &SignalEmitter<'_>, shifted: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn key_captured(emitter: &SignalEmitter<'_>, key: String) -> zbus::Result<()>;
}

/// Claim the bus name and export the interface. Fails if another instance owns the
/// name, which doubles as the single-instance guard.
pub async fn serve(cmds: mpsc::Sender<Command>) -> zbus::Result<Connection> {
    zbus::connection::Builder::session()
        .name(BUS_NAME)
        .serve_at(OBJECT_PATH, Service { cmds })
        .build()
        .await
}

/// Emit signals in order until the sender side is dropped.
pub async fn run_signals(conn: Connection, mut rx: mpsc::UnboundedReceiver<Signal>) {
    let iface = match conn
        .object_server()
        .interface::<_, Service>(OBJECT_PATH)
        .await
    {
        Ok(i) => i,
        Err(e) => {
            warn!("D-Bus signals disabled: {e}");
            return;
        }
    };
    while let Some(sig) = rx.recv().await {
        let e = iface.signal_emitter();
        let result = match sig {
            Signal::ProfileChanged(n) => Service::profile_changed(e, n).await,
            Signal::ModeChanged(m) => Service::mode_changed(e, m).await,
            Signal::ShiftChanged(s) => Service::shift_changed(e, s).await,
            Signal::KeyCaptured(k) => Service::key_captured(e, k).await,
        };
        if let Err(err) = result {
            warn!("emitting D-Bus signal failed: {err}");
        }
    }
}
