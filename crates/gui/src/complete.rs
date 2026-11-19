//! Key-name completion for chord entry. Pure.
//!
//! Chords are written `KEY_LEFTCTRL+KEY_C`. Typing the short, human form ("shift", "ctrl",
//! "esc") finds the real names, and the segment being typed is the one after the last `+`.

use tartarusd_core::Key;

/// Words people type that aren't a substring of the Linux name.
const ALIASES: &[(&str, &str)] = &[
    ("SUPER", "META"),
    ("WIN", "META"),
    ("WINDOWS", "META"),
    ("CONTROL", "CTRL"),
    ("ESCAPE", "ESC"),
    ("RETURN", "ENTER"),
    ("DEL", "DELETE"),
    ("PGUP", "PAGEUP"),
    ("PGDN", "PAGEDOWN"),
    ("MIDDLE", "BTN_MIDDLE"),
];

/// Split chord text into what's already settled and the segment being typed.
pub fn split_last(text: &str) -> (&str, &str) {
    match text.rfind('+') {
        Some(i) => (&text[..=i], text[i + 1..].trim_start()),
        None => ("", text.trim_start()),
    }
}

/// Whether `segment` is already a complete, known key name.
pub fn is_complete(segment: &str) -> bool {
    Key::from_name(segment.trim()).is_some()
}

/// Up to `limit` key names matching what was typed, best first: exact, then prefix, then
/// substring, shorter names before longer. Case-insensitive; `KEY_` is optional.
pub fn suggest(typed: &str, limit: usize) -> Vec<&'static str> {
    let q = typed.trim().to_ascii_uppercase();
    let q = q.strip_prefix("KEY_").unwrap_or(&q);
    if q.is_empty() {
        return Vec::new();
    }
    let alias = ALIASES.iter().find(|(a, _)| *a == q).map(|(_, n)| *n);
    let mut hits: Vec<(u8, usize, &'static str)> = Vec::new();
    for (_, name) in Key::all_named() {
        let short = name.strip_prefix("KEY_").unwrap_or(name);
        let score = |q: &str| {
            if short == q || name == q {
                Some(0)
            } else if short.starts_with(q) {
                Some(1)
            } else if short.contains(q) {
                Some(2)
            } else {
                None
            }
        };
        let best = score(q).or_else(|| alias.and_then(|a| score(a).map(|s| s + 1)));
        if let Some(s) = best {
            hits.push((s, short.len(), name));
        }
    }
    hits.sort();
    hits.dedup_by_key(|h| h.2);
    hits.into_iter().take(limit).map(|h| h.2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_finds_both_shift_keys() {
        let s = suggest("Shift", 8);
        assert_eq!(&s[..2], ["KEY_LEFTSHIFT", "KEY_RIGHTSHIFT"]);
    }

    #[test]
    fn a_single_letter_ranks_the_letter_first() {
        assert_eq!(suggest("a", 8)[0], "KEY_A");
        assert_eq!(suggest("KEY_a", 8)[0], "KEY_A");
    }

    #[test]
    fn prefix_beats_substring() {
        let s = suggest("page", 8);
        assert!(s[0].starts_with("KEY_PAGE"), "{s:?}");
        assert!(s.contains(&"KEY_PAGEUP") && s.contains(&"KEY_PAGEDOWN"));
    }

    #[test]
    fn aliases_reach_the_linux_names() {
        assert!(suggest("super", 8).contains(&"KEY_LEFTMETA"));
        assert!(suggest("ctrl", 8).contains(&"KEY_LEFTCTRL"));
        assert!(suggest("control", 8).contains(&"KEY_LEFTCTRL"));
        assert!(suggest("esc", 8).contains(&"KEY_ESC"));
        assert!(suggest("return", 8).contains(&"KEY_ENTER"));
    }

    #[test]
    fn exact_names_come_first_and_nothing_matches_gibberish() {
        assert_eq!(suggest("KEY_ESC", 8)[0], "KEY_ESC");
        assert!(suggest("zzzzqq", 8).is_empty());
        assert!(suggest("  ", 8).is_empty());
    }

    #[test]
    fn results_are_capped_and_unique() {
        let s = suggest("e", 5);
        assert_eq!(s.len(), 5);
        let mut d = s.clone();
        d.dedup();
        assert_eq!(s, d);
    }

    #[test]
    fn the_last_segment_is_the_one_being_typed() {
        assert_eq!(split_last("KEY_LEFTCTRL+sh"), ("KEY_LEFTCTRL+", "sh"));
        assert_eq!(split_last("sh"), ("", "sh"));
        assert_eq!(split_last("KEY_A+"), ("KEY_A+", ""));
        assert!(is_complete("KEY_A"));
        assert!(!is_complete("sh"));
    }
}
