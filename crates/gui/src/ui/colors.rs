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
.swatch { min-width: 26px; min-height: 26px; padding: 0; border-radius: 6px; }
.rubber { background: alpha(@accent_bg_color, 0.25); border: 1px solid @accent_bg_color; }
.mode-key { min-width: 54px; min-height: 54px; opacity: 0.45; }
";

/// Quick colors offered by the picker.
pub const PALETTE: [Rgb; 12] = [
    Rgb(255, 0, 0),
    Rgb(255, 128, 0),
    Rgb(255, 255, 0),
    Rgb(0, 255, 0),
    Rgb(0, 255, 255),
    Rgb(0, 0, 255),
    Rgb(128, 0, 255),
    Rgb(255, 0, 255),
    Rgb(255, 80, 144),
    Rgb(255, 255, 255),
    Rgb(255, 208, 160),
    Rgb(0, 0, 0),
];

pub fn key(c: Rgb) -> u32 {
    u32::from(c.0) << 16 | u32::from(c.1) << 8 | u32::from(c.2)
}

/// The CSS class that paints a button in `c`.
pub fn class_name(c: Rgb) -> String {
    format!("c-{:06x}", key(c))
}

fn rules(known: &BTreeSet<u32>) -> String {
    let mut css = String::from(BASE_CSS);
    for n in known {
        let c = Rgb((*n >> 16) as u8, (*n >> 8) as u8, *n as u8);
        css.push_str(&format!(
            "button.c-{n:06x} {{ background: #{n:06x}; color: {}; }}\n",
            readable_on(c)
        ));
    }
    css
}

pub struct Colors {
    provider: gtk::CssProvider,
    known: RefCell<BTreeSet<u32>>,
}

impl Colors {
    pub fn install() -> Colors {
        let known: BTreeSet<u32> = PALETTE.iter().map(|c| key(*c)).collect();
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&rules(&known));
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        Colors {
            provider,
            known: RefCell::new(known),
        }
    }

    /// The CSS class that paints a key cap in `c`; registers the color on first use.
    pub fn class(&self, c: Rgb) -> String {
        if self.known.borrow_mut().insert(key(c)) {
            self.provider.load_from_string(&rules(&self.known.borrow()));
        }
        class_name(c)
    }

    /// Forget colors no cap shows any more, so dragging a slider doesn't grow the style sheet.
    pub fn prune(&self, used: &BTreeSet<u32>) {
        let keep: BTreeSet<u32> = PALETTE
            .iter()
            .map(|c| key(*c))
            .chain(used.iter().copied())
            .collect();
        if *self.known.borrow() != keep {
            self.provider.load_from_string(&rules(&keep));
            *self.known.borrow_mut() = keep;
        }
    }
}

/// Black or white text, whichever reads better on `c`.
pub fn readable_on(c: Rgb) -> &'static str {
    let lum = 0.299 * f32::from(c.0) + 0.587 * f32::from(c.1) + 0.114 * f32::from(c.2);
    if lum > 140.0 { "#000000" } else { "#ffffff" }
}

/// `(hue 0..360, saturation 0..1, value 0..1)`.
pub fn rgb_to_hsv(c: Rgb) -> (f32, f32, f32) {
    let (r, g, b) = (
        f32::from(c.0) / 255.0,
        f32::from(c.1) / 255.0,
        f32::from(c.2) / 255.0,
    );
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Rgb {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let byte = |f: f32| ((f + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb(byte(r), byte(g), byte(b))
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
    fn hsv_round_trips_closely_and_knows_the_primaries() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), Rgb(255, 0, 0));
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), Rgb(0, 255, 0));
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), Rgb(0, 0, 255));
        assert_eq!(hsv_to_rgb(0.0, 0.0, 1.0), Rgb(255, 255, 255));
        assert_eq!(rgb_to_hsv(Rgb(0, 0, 0)), (0.0, 0.0, 0.0));
        for (r, g, b) in [
            (12, 200, 99),
            (255, 128, 0),
            (3, 3, 250),
            (77, 77, 77),
            (200, 10, 120),
        ] {
            let (h, s, v) = rgb_to_hsv(Rgb(r, g, b));
            let Rgb(r2, g2, b2) = hsv_to_rgb(h, s, v);
            assert!(
                r.abs_diff(r2) <= 1 && g.abs_diff(g2) <= 1 && b.abs_diff(b2) <= 1,
                "{r},{g},{b}"
            );
        }
    }
}
