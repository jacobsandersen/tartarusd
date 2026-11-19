//! Hyprland window source: the event socket (`.socket2.sock`) for focus changes and
//! the request socket (`.socket.sock`) for the window that is focused at startup.

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use tartarusd_core::ActiveWindow;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::mpsc,
};
use tracing::{debug, warn};

const RETRY: Duration = Duration::from_secs(2);

pub struct Hyprland {
    events: PathBuf,
    requests: PathBuf,
}

/// Locate the running instance: `$HYPRLAND_INSTANCE_SIGNATURE` if set (and valid),
/// otherwise the most recently started instance under `$XDG_RUNTIME_DIR/hypr`.
pub fn detect() -> Option<Hyprland> {
    let hypr = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("hypr");
    let dir = match std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") {
        Some(sig) if hypr.join(&sig).join(".socket2.sock").exists() => hypr.join(sig),
        _ => newest_instance(&hypr)?,
    };
    Some(Hyprland {
        events: dir.join(".socket2.sock"),
        requests: dir.join(".socket.sock"),
    })
}

fn newest_instance(hypr: &Path) -> Option<PathBuf> {
    std::fs::read_dir(hypr)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let modified = e
                .path()
                .join(".socket2.sock")
                .metadata()
                .ok()?
                .modified()
                .ok()?;
            Some((modified, e.path()))
        })
        .max()
        .map(|(_, p)| p)
}

/// `activewindow>>CLASS,TITLE`. The title may itself contain commas, so split once.
/// Other events (including `activewindowv2`) yield `None`.
pub fn parse_event(line: &str) -> Option<ActiveWindow> {
    let rest = line.strip_prefix("activewindow>>")?;
    let (class, title) = rest.split_once(',').unwrap_or((rest, ""));
    Some(ActiveWindow {
        class: class.into(),
        title: title.into(),
    })
}

/// Reply to `j/activewindow`: an object with `class`/`title`, or an empty object /
/// error text when nothing is focused (-> the empty window).
pub fn parse_active_window(reply: &str) -> ActiveWindow {
    let v: serde_json::Value = serde_json::from_str(reply).unwrap_or_default();
    let field = |k: &str| {
        v.get(k)
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    ActiveWindow {
        class: field("class"),
        title: field("title"),
    }
}

impl Hyprland {
    /// Forward focus changes until the receiver is dropped, reconnecting if Hyprland
    /// goes away (for example across a compositor restart).
    pub async fn run(self, tx: mpsc::Sender<ActiveWindow>) {
        loop {
            match self.session(&tx).await {
                Ok(()) => return, // receiver dropped: daemon is shutting down
                Err(e) => warn!("hyprland connection lost: {e}; retrying"),
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    async fn query_active(&self) -> io::Result<ActiveWindow> {
        let mut s = UnixStream::connect(&self.requests).await?;
        s.write_all(b"j/activewindow").await?;
        let mut reply = String::new();
        s.read_to_string(&mut reply).await?;
        Ok(parse_active_window(&reply))
    }

    /// One connection's lifetime. `Ok` means the receiver is gone; `Err` means retry.
    async fn session(&self, tx: &mpsc::Sender<ActiveWindow>) -> io::Result<()> {
        // Subscribe before asking for the current window so no focus change is missed.
        let events = UnixStream::connect(&self.events).await?;
        let first = self.query_active().await?;
        debug!(class = %first.class, "hyprland connected");
        if tx.send(first).await.is_err() {
            return Ok(());
        }
        let mut lines = BufReader::new(events).lines();
        while let Some(line) = lines.next_line().await? {
            if let Some(w) = parse_event(&line)
                && tx.send(w).await.is_err()
            {
                return Ok(());
            }
        }
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "event socket closed",
        ))
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::UnixListener;

    use super::*;

    fn w(class: &str, title: &str) -> ActiveWindow {
        ActiveWindow {
            class: class.into(),
            title: title.into(),
        }
    }

    #[test]
    fn parses_events() {
        assert_eq!(
            parse_event("activewindow>>kitty,zsh"),
            Some(w("kitty", "zsh"))
        );
        assert_eq!(
            parse_event("activewindow>>steam_app_1245620,ELDEN RING, the game"),
            Some(w("steam_app_1245620", "ELDEN RING, the game")),
            "only the first comma splits"
        );
        assert_eq!(parse_event("activewindow>>,"), Some(w("", "")), "no focus");
        assert_eq!(parse_event("activewindow>>kitty"), Some(w("kitty", "")));
        assert_eq!(parse_event("activewindowv2>>0x1234"), None);
        assert_eq!(parse_event("workspace>>2"), None);
    }

    #[test]
    fn parses_query_reply() {
        let json = r#"{"address":"0x1","class":"kitty","title":"hello","pid":1}"#;
        assert_eq!(parse_active_window(json), w("kitty", "hello"));
        assert_eq!(parse_active_window("{}"), w("", ""));
        assert_eq!(parse_active_window("Invalid"), w("", ""));
    }

    #[test]
    fn newest_instance_prefers_the_latest_live_socket() {
        let d = tempfile::tempdir().unwrap();
        for n in ["old", "stale"] {
            std::fs::create_dir(d.path().join(n)).unwrap();
        }
        std::fs::write(d.path().join("old/.socket2.sock"), "").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::create_dir(d.path().join("new")).unwrap();
        std::fs::write(d.path().join("new/.socket2.sock"), "").unwrap();
        assert_eq!(newest_instance(d.path()).unwrap(), d.path().join("new"));
        assert!(newest_instance(&d.path().join("missing")).is_none());
    }

    #[tokio::test]
    async fn streams_initial_and_changed_windows() {
        let d = tempfile::tempdir().unwrap();
        let events = UnixListener::bind(d.path().join("ev")).unwrap();
        let requests = UnixListener::bind(d.path().join("rq")).unwrap();

        tokio::spawn(async move {
            let (mut s, _) = events.accept().await.unwrap();
            s.write_all(
                b"workspace>>1\nactivewindow>>game,Level 1, boss\nactivewindowv2>>0xabc\nactivewindow>>,\n",
            )
            .await
            .unwrap();
            std::future::pending::<()>().await; // keep the connection open
        });
        tokio::spawn(async move {
            let (mut s, _) = requests.accept().await.unwrap();
            let mut req = [0u8; 64];
            let n = s.read(&mut req).await.unwrap();
            assert_eq!(&req[..n], b"j/activewindow");
            s.write_all(br#"{"class":"kitty","title":"zsh"}"#)
                .await
                .unwrap();
        });

        let src = Hyprland {
            events: d.path().join("ev"),
            requests: d.path().join("rq"),
        };
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(src.run(tx));
        let mut got = Vec::new();
        for _ in 0..3 {
            let next = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;
            got.push(next.expect("timed out").expect("channel closed"));
        }
        assert_eq!(
            got,
            [w("kitty", "zsh"), w("game", "Level 1, boss"), w("", "")]
        );
        task.abort();
    }

    /// Needs a running Hyprland; read-only. Run with `cargo test --features hyprland -- --ignored`.
    #[tokio::test]
    #[ignore = "requires a running Hyprland session"]
    async fn live_reports_the_focused_window() {
        let src = detect().expect("no Hyprland instance detected");
        let (tx, mut rx) = mpsc::channel(4);
        let task = tokio::spawn(src.run(tx));
        let first = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        eprintln!(
            "focused window: class={:?} title={:?}",
            first.class, first.title
        );
        assert!(
            !first.class.is_empty(),
            "expected a focused window with a class"
        );
        task.abort();
    }
}
