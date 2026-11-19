//! Reading the profiles directory and watching it for changes.

use std::{io, path::Path, time::Duration};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::warn;

/// Debounce between the last filesystem event and a reload; lets an editor's or the
/// GUI's write-then-rename settle.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// `(file name, contents)` for every `*.toml` in `dir`. Unreadable files are skipped
/// with a warning rather than failing the whole reload.
pub fn read_files(dir: &Path) -> io::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "toml") || !path.is_file() {
            continue;
        }
        let label = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match std::fs::read_to_string(&path) {
            Ok(text) => out.push((label, text)),
            Err(e) => warn!(file = %path.display(), "cannot read profile: {e}"),
        }
    }
    Ok(out)
}

/// Watch `dir` (non-recursive). Sends `()` for every relevant change. Keep the returned
/// watcher alive; dropping it stops the watch.
pub fn watch(
    dir: &Path,
) -> notify::Result<(notify::RecommendedWatcher, mpsc::UnboundedReceiver<()>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        let changed = matches!(
            ev.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        );
        if changed
            && ev
                .paths
                .iter()
                .any(|p| p.extension().is_some_and(|e| e == "toml"))
        {
            let _ = tx.send(());
        }
    })?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok((watcher, rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_toml_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.toml"), "x").unwrap();
        std::fs::write(d.path().join("notes.txt"), "y").unwrap();
        std::fs::write(d.path().join("b.toml.tmp"), "z").unwrap();
        std::fs::create_dir(d.path().join("sub.toml")).unwrap();
        assert_eq!(
            read_files(d.path()).unwrap(),
            [("a.toml".to_string(), "x".to_string())]
        );
    }

    async fn notified(rx: &mut mpsc::UnboundedReceiver<()>) -> bool {
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn notifies_on_create_atomic_rename_and_remove() {
        let d = tempfile::tempdir().unwrap();
        let (_w, mut rx) = watch(d.path()).unwrap();

        std::fs::write(d.path().join("a.toml"), "1").unwrap();
        assert!(notified(&mut rx).await, "create");
        while rx.try_recv().is_ok() {}

        // GUI-style atomic save: write a temp file, then rename over the target.
        std::fs::write(d.path().join(".a.toml.tmp"), "2").unwrap();
        std::fs::rename(d.path().join(".a.toml.tmp"), d.path().join("a.toml")).unwrap();
        assert!(notified(&mut rx).await, "rename over");
        while rx.try_recv().is_ok() {}

        std::fs::remove_file(d.path().join("a.toml")).unwrap();
        assert!(notified(&mut rx).await, "remove");
    }

    #[tokio::test]
    async fn ignores_non_toml_files() {
        let d = tempfile::tempdir().unwrap();
        let (_w, mut rx) = watch(d.path()).unwrap();
        std::fs::write(d.path().join("scratch.swp"), "1").unwrap();
        assert!(
            !tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .is_ok()
        );
    }
}
