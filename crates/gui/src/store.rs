//! The profiles directory: listing, saving and deleting profile files.
//!
//! Files are the source of truth and the daemon reloads them when they change, so saving
//! is a write-then-rename: the daemon never sees a half-written file.

use std::{
    io,
    path::{Path, PathBuf},
};

use anyhow::Context;
use tartarusd_core::{Profile, ProfileError};

/// `$XDG_CONFIG_HOME/tartarusd/profiles`, the same default the daemon uses.
pub fn default_dir() -> anyhow::Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(
            std::env::var_os("HOME").context("neither XDG_CONFIG_HOME nor HOME is set")?,
        )
        .join(".config"),
    };
    Ok(base.join("tartarusd/profiles"))
}

/// One `*.toml` file in the directory.
#[derive(Debug)]
pub struct Entry {
    pub file: String,
    /// The parsed profile, or why the file doesn't validate.
    pub profile: Result<Profile, String>,
}

/// Every `*.toml` in `dir`, sorted by file name. A missing directory is an empty list.
pub fn list(dir: &Path) -> io::Result<Vec<Entry>> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in read {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "toml") || !path.is_file() {
            continue;
        }
        let file = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let profile = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| Profile::from_toml(&t).map_err(|e| e.to_string()));
        out.push(Entry { file, profile });
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(out)
}

/// The check every save goes through: what we write must load back as the same profile.
pub fn validate(p: &Profile) -> Result<(), ProfileError> {
    Profile::from_toml(&p.to_toml()).map(|_| ())
}

/// Write `profile` to `dir/file` atomically, creating `dir` if needed. Refuses a profile
/// that wouldn't load.
pub fn save(dir: &Path, file: &str, profile: &Profile) -> anyhow::Result<()> {
    validate(profile)?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(file);
    let tmp = dir.join(format!(".{file}.tmp"));
    std::fs::write(&tmp, profile.to_toml())
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))
}

pub fn delete(dir: &Path, file: &str) -> io::Result<()> {
    std::fs::remove_file(dir.join(file))
}

/// A file name for a new profile called `name`: a lowercase slug ending in `.toml`, made
/// unique against `taken`.
pub fn file_name_for(name: &str, taken: &[String]) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "profile" } else { slug };
    let mut candidate = format!("{slug}.toml");
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{slug}-{n}.toml");
        n += 1;
    }
    candidate
}

/// Whether another file already holds a profile called `name` (the daemon rejects
/// duplicate names).
pub fn name_taken(entries: &[Entry], name: &str, except_file: Option<&str>) -> bool {
    entries.iter().any(|e| {
        Some(e.file.as_str()) != except_file && e.profile.as_ref().is_ok_and(|p| p.name == name)
    })
}

/// A minimal valid profile: one empty mode, nothing bound.
pub fn blank(name: &str) -> Profile {
    Profile::from_toml(&format!("name = {name:?}\n[mode.0]\n")).expect("a blank profile is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_slugs_and_unique() {
        assert_eq!(file_name_for("Elden Ring", &[]), "elden-ring.toml");
        assert_eq!(file_name_for("  !!  ", &[]), "profile.toml");
        assert_eq!(file_name_for("A/B: C", &[]), "a-b-c.toml");
        let taken = vec!["elden-ring.toml".into(), "elden-ring-2.toml".into()];
        assert_eq!(file_name_for("Elden Ring", &taken), "elden-ring-3.toml");
    }

    #[test]
    fn save_then_list_round_trips_and_leaves_no_temp_file() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("nested/profiles");
        let p = blank("Smoke \"quoted\"");
        save(&dir, "a.toml", &p).unwrap();
        save(&dir, "a.toml", &p).unwrap();
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["a.toml"]);
        let entries = list(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].profile.as_ref().unwrap(), &p);
    }

    #[test]
    fn invalid_files_are_listed_with_their_error_and_missing_dir_is_empty() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("bad.toml"), "name = \"x\"\n").unwrap();
        std::fs::write(d.path().join("notes.txt"), "x").unwrap();
        let entries = list(d.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0]
                .profile
                .as_ref()
                .unwrap_err()
                .contains("no modes")
        );
        assert!(list(&d.path().join("absent")).unwrap().is_empty());
    }

    #[test]
    fn save_refuses_a_profile_that_would_not_load() {
        let d = tempfile::tempdir().unwrap();
        let mut p = blank("x");
        p.name.clear();
        assert!(save(d.path(), "x.toml", &p).is_err());
        assert!(!d.path().join("x.toml").exists());
    }

    #[test]
    fn name_taken_ignores_the_file_being_edited() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), "a.toml", &blank("A")).unwrap();
        save(d.path(), "b.toml", &blank("B")).unwrap();
        let entries = list(d.path()).unwrap();
        assert!(name_taken(&entries, "A", None));
        assert!(!name_taken(&entries, "A", Some("a.toml")));
        assert!(name_taken(&entries, "B", Some("a.toml")));
        assert!(!name_taken(&entries, "C", None));
    }
}
