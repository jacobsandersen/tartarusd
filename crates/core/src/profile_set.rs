//! The set of profiles loaded from a directory, with reload semantics.
//!
//! Pure: callers pass `(label, toml text)` pairs (label = file name) and get back a
//! report. A file that stops validating keeps its previous good version; a file that
//! never validated is simply absent. No partial state is ever exposed.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use crate::profile::{Profile, ProfileError};

#[derive(Debug, thiserror::Error)]
pub enum SetError {
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error("profile name `{0}` is already used by another file")]
    DuplicateName(String),
}

#[derive(Debug)]
pub struct LoadError {
    pub label: String,
    pub error: SetError,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PickError {
    #[error("no profiles found")]
    NoProfiles,
    #[error("profile `{name}` not found (available: {})", available.join(", "))]
    NotFound {
        name: String,
        available: Vec<String>,
    },
    #[error("several profiles exist ({}); set `default_profile` in config.toml", available.join(", "))]
    Ambiguous { available: Vec<String> },
}

#[derive(Default, Debug)]
pub struct ProfileSet {
    /// Keyed by file label, so a file keeps its slot across edits.
    entries: BTreeMap<String, Arc<Profile>>,
}

impl ProfileSet {
    pub fn new() -> ProfileSet {
        ProfileSet::default()
    }

    /// Rebuild from the directory's current contents. Files absent from `files` are
    /// dropped. Unchanged profiles keep their `Arc`.
    pub fn update(&mut self, files: &[(String, String)]) -> Vec<LoadError> {
        let mut files: Vec<&(String, String)> = files.iter().collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let live: HashSet<&str> = files.iter().map(|(l, _)| l.as_str()).collect();
        self.entries
            .retain(|label, _| live.contains(label.as_str()));

        let mut errors = Vec::new();
        let mut next: BTreeMap<String, Arc<Profile>> = BTreeMap::new();
        let mut taken: HashMap<String, String> = HashMap::new(); // name -> label

        for (label, text) in files {
            let old = self.entries.get(label).cloned();
            let candidate = match Profile::from_toml(text) {
                Ok(p) => match old {
                    Some(o) if *o == p => Some(o), // unchanged: keep the Arc
                    _ => Some(Arc::new(p)),
                },
                Err(e) => {
                    errors.push(LoadError {
                        label: label.clone(),
                        error: e.into(),
                    });
                    old // keep the last good version, if any
                }
            };
            let Some(mut chosen) = candidate else {
                continue;
            };
            if taken.contains_key(&chosen.name) {
                errors.push(LoadError {
                    label: label.clone(),
                    error: SetError::DuplicateName(chosen.name.clone()),
                });
                // Fall back to the previous version if its name is still free.
                match self.entries.get(label) {
                    Some(o) if !taken.contains_key(&o.name) => chosen = o.clone(),
                    _ => continue,
                }
            }
            taken.insert(chosen.name.clone(), label.clone());
            next.insert(label.clone(), chosen);
        }
        self.entries = next;
        errors
    }

    pub fn get(&self, name: &str) -> Option<&Arc<Profile>> {
        self.entries.values().find(|p| p.name == name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<String> = self.entries.values().map(|p| p.name.clone()).collect();
        n.sort();
        n
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The new version of `current` (looked up by name) if it differs, else `None`.
    /// `None` also covers "no longer present": the caller keeps running `current`.
    pub fn updated(&self, current: &Profile) -> Option<Arc<Profile>> {
        self.get(&current.name)
            .filter(|p| ***p != *current)
            .cloned()
    }

    /// Choose the startup profile: `preferred` by name, else the only one.
    pub fn pick(&self, preferred: Option<&str>) -> Result<Arc<Profile>, PickError> {
        if self.is_empty() {
            return Err(PickError::NoProfiles);
        }
        match preferred {
            Some(name) => self.get(name).cloned().ok_or_else(|| PickError::NotFound {
                name: name.to_owned(),
                available: self.names(),
            }),
            None if self.len() == 1 => Ok(self.entries.values().next().expect("len 1").clone()),
            None => Err(PickError::Ambiguous {
                available: self.names(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(label: &str, name: &str, extra: &str) -> (String, String) {
        (label.into(), format!("name = '{name}'\n[mode.0]\n{extra}"))
    }

    #[test]
    fn loads_valid_files_and_reports_invalid() {
        let mut s = ProfileSet::new();
        let errs = s.update(&[f("a.toml", "A", ""), ("bad.toml".into(), "name = ".into())]);
        assert_eq!(s.names(), ["A"]);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].label, "bad.toml");
    }

    #[test]
    fn invalid_edit_keeps_previous_good_version() {
        let mut s = ProfileSet::new();
        s.update(&[f("a.toml", "A", "KEY_1 = 'KEY_A'\n")]);
        let before = s.get("A").unwrap().clone();
        let errs = s.update(&[f("a.toml", "A", "KEY_1 = 'KEY_NOPE'\n")]);
        assert_eq!(errs.len(), 1);
        assert!(Arc::ptr_eq(&before, s.get("A").unwrap()));
    }

    #[test]
    fn valid_edit_replaces_and_unchanged_keeps_arc() {
        let mut s = ProfileSet::new();
        s.update(&[f("a.toml", "A", "KEY_1 = 'KEY_A'\n")]);
        let v1 = s.get("A").unwrap().clone();
        assert!(
            s.update(&[f("a.toml", "A", "KEY_1 = 'KEY_A'\n")])
                .is_empty()
        );
        assert!(
            Arc::ptr_eq(&v1, s.get("A").unwrap()),
            "unchanged file keeps its Arc"
        );
        s.update(&[f("a.toml", "A", "KEY_1 = 'KEY_B'\n")]);
        assert!(!Arc::ptr_eq(&v1, s.get("A").unwrap()));
    }

    #[test]
    fn removed_files_disappear() {
        let mut s = ProfileSet::new();
        s.update(&[f("a.toml", "A", ""), f("b.toml", "B", "")]);
        s.update(&[f("b.toml", "B", "")]);
        assert_eq!(s.names(), ["B"]);
    }

    #[test]
    fn duplicate_names_rejected_deterministically() {
        let mut s = ProfileSet::new();
        let errs = s.update(&[f("b.toml", "Same", ""), f("a.toml", "Same", "")]);
        assert_eq!(s.len(), 1);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].label, "b.toml", "the later label loses");
        assert!(matches!(errs[0].error, SetError::DuplicateName(_)));
    }

    #[test]
    fn updated_only_when_content_differs() {
        let mut s = ProfileSet::new();
        s.update(&[f("a.toml", "A", "KEY_1 = 'KEY_A'\n")]);
        let cur = s.get("A").unwrap().clone();
        s.update(&[f("a.toml", "A", "KEY_1 = 'KEY_A'\n")]);
        assert!(s.updated(&cur).is_none());
        s.update(&[f("a.toml", "A", "KEY_1 = 'KEY_B'\n")]);
        assert_eq!(s.updated(&cur).unwrap().modes[0].bindings.len(), 1);
        s.update(&[]);
        assert!(
            s.updated(&cur).is_none(),
            "removed: keep running the current one"
        );
    }

    #[test]
    fn pick_rules() {
        let mut s = ProfileSet::new();
        assert_eq!(s.pick(None).unwrap_err(), PickError::NoProfiles);
        s.update(&[f("a.toml", "A", "")]);
        assert_eq!(s.pick(None).unwrap().name, "A");
        s.update(&[f("a.toml", "A", ""), f("b.toml", "B", "")]);
        assert!(matches!(s.pick(None), Err(PickError::Ambiguous { .. })));
        assert_eq!(s.pick(Some("B")).unwrap().name, "B");
        assert!(matches!(s.pick(Some("Z")), Err(PickError::NotFound { .. })));
    }
}
