//! Locate the Tartarus event nodes via stable `/dev/input/by-id` paths.

use std::{
    io,
    path::{Path, PathBuf},
};

pub const BY_ID_DIR: &str = "/dev/input/by-id";
const PREFIX: &str = "usb-Razer_Razer_Tartarus_V2-";

/// All Tartarus event nodes under `dir` (if00 kbd, if01 kbd, if02 mouse), sorted.
pub fn find_nodes(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(PREFIX) && name.contains("-event-") {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_tartarus_event_nodes() {
        let d = tempfile::tempdir().unwrap();
        for n in [
            "usb-Razer_Razer_Tartarus_V2-event-kbd",
            "usb-Razer_Razer_Tartarus_V2-if01-event-kbd",
            "usb-Razer_Razer_Tartarus_V2-if02-event-mouse",
            "usb-Razer_Razer_Tartarus_V2-if02-mouse", // not an event node
            "usb-Other_Keyboard-event-kbd",
            "usb-Razer_Razer_Tartarus_V2_Pro-event-kbd", // different device
        ] {
            std::fs::File::create(d.path().join(n)).unwrap();
        }
        let got: Vec<String> = find_nodes(d.path())
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            got,
            [
                "usb-Razer_Razer_Tartarus_V2-event-kbd",
                "usb-Razer_Razer_Tartarus_V2-if01-event-kbd",
                "usb-Razer_Razer_Tartarus_V2-if02-event-mouse",
            ]
        );
    }
}
