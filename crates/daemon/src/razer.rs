//! The OpenRazer daemon's D-Bus device object for the Tartarus V2.
//! OpenRazer is required; sysfs is never touched (docs/hardware.md, "OpenRazer D-Bus path").

use anyhow::{Context, bail};
use tracing::debug;
use zbus::{Connection, Proxy};

const DEST: &str = "org.razer";
const VID_PID: [i32; 2] = [0x1532, 0x022B];

pub struct Razer {
    conn: Connection,
    /// `/org/razer/device/<serial>`; the serial is discovered, never hard-coded.
    path: String,
}

impl Razer {
    /// Connect to openrazer-daemon and find the Tartarus V2 by VID:PID.
    pub async fn connect() -> anyhow::Result<Razer> {
        let conn = Connection::session()
            .await
            .context("connecting to the session bus")?;
        let devices = Proxy::new(&conn, DEST, "/org/razer", "razer.devices")
            .await
            .context("creating OpenRazer proxy")?;
        let serials: Vec<String> = devices.call("getDevices", &()).await.context(
            "openrazer-daemon is required but did not answer on org.razer (is it running?)",
        )?;
        for serial in serials {
            let path = format!("/org/razer/device/{serial}");
            let misc = Proxy::new(&conn, DEST, path.clone(), "razer.device.misc").await?;
            let vid_pid: Vec<i32> = misc.call("getVidPid", &()).await?;
            if vid_pid == VID_PID {
                debug!(%serial, "found Tartarus V2 in openrazer-daemon");
                return Ok(Razer { conn, path });
            }
        }
        bail!("openrazer-daemon is running but does not list a Razer Tartarus V2 (1532:022b)")
    }

    /// A proxy for one of the device's interfaces.
    pub async fn proxy(&self, interface: &'static str) -> zbus::Result<Proxy<'static>> {
        Proxy::new(&self.conn, DEST, self.path.clone(), interface).await
    }
}
