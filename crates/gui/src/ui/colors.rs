//! Styling: the key cap look, and one CSS class per lighting color in use.

use std::{cell::RefCell, collections::BTreeSet};

use gtk::gdk;
use tartarusd_core::Rgb;

const BASE_CSS: &str = "
.keycap { padding: 4px 6px; min-width: 54px; min-height: 54px; }
.keycap .pad-label { font-size: 0.75em; opacity: 0.65; }
.keycap .bind-label { font-weight: bold; }
.keycap.unbound .bind-label { opacity: 0.45; font-weight: normal; }
.keycap.selected { outline: 3px solid @accent_bg_color; outline-offset: -3px; }
.keycap.no-led { opacity: 0.4; }
.keycap.lit .pad-label { opacity: 0.75; }
.mode-key { min-width: 54px; min-height: 54px; opacity: 0.45; }
";

pub struct Colors {
    provider: gtk::CssProvider,
    known: RefCell<BTreeSet<u32>>,
}

impl Colors {
    pub fn install() -> Colors {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(BASE_CSS);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        Colors {
            provider,
            known: RefCell::default(),
        }
    }

    /// The CSS class that paints a cap in `c`; registers the color on first use.
    pub fn class(&self, c: Rgb) -> String {
        let n = u32::from(c.0) << 16 | u32::from(c.1) << 8 | u32::from(c.2);
        if self.known.borrow_mut().insert(n) {
            let mut css = String::from(BASE_CSS);
            for n in self.known.borrow().iter() {
                css.push_str(&format!(
                    ".keycap.c-{n:06x} {{ background: #{n:06x}; color: {}; }}\n",
                    readable_on(Rgb((*n >> 16) as u8, (*n >> 8) as u8, *n as u8))
                ));
            }
            self.provider.load_from_string(&css);
        }
        format!("c-{n:06x}")
    }
}

/// Black or white text, whichever reads better on `c`.
pub fn readable_on(c: Rgb) -> &'static str {
    let lum = 0.299 * f32::from(c.0) + 0.587 * f32::from(c.1) + 0.114 * f32::from(c.2);
    if lum > 140.0 { "#000000" } else { "#ffffff" }
}

pub fn to_rgba(c: Rgb) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(c.0) / 255.0,
        f32::from(c.1) / 255.0,
        f32::from(c.2) / 255.0,
        1.0,
    )
}

pub fn from_rgba(c: &gdk::RGBA) -> Rgb {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb(b(c.red()), b(c.green()), b(c.blue()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_contrasts_with_the_background() {
        assert_eq!(readable_on(Rgb(0, 0, 0)), "#ffffff");
        assert_eq!(readable_on(Rgb(255, 255, 255)), "#000000");
        assert_eq!(readable_on(Rgb(255, 0, 0)), "#ffffff");
        assert_eq!(readable_on(Rgb(0, 255, 255)), "#000000");
    }

    #[test]
    fn rgba_round_trips_every_byte() {
        for v in 0..=255u8 {
            assert_eq!(
                from_rgba(&to_rgba(Rgb(v, 255 - v, v / 2))),
                Rgb(v, 255 - v, v / 2)
            );
        }
    }
}
