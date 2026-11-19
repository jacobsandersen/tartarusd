//! A color picker that reports every change as it happens: swatches, hue/saturation/
//! brightness sliders and a hex field. It is built from plain widgets (not the color
//! dialog) so that dragging a slider can update the keys and the pad live.

use std::{cell::Cell, rc::Rc, str::FromStr};

use gtk::prelude::*;
use tartarusd_core::Rgb;

use super::colors::{self, PALETTE};

struct Inner {
    entry: gtk::Entry,
    /// Hue (0..360), saturation and brightness (0..100).
    scales: [gtk::Scale; 3],
    updating: Cell<bool>,
    on_change: Rc<dyn Fn(Rgb)>,
}

impl Inner {
    fn show(&self, c: Rgb) {
        self.updating.set(true);
        self.entry.set_text(&c.to_string());
        self.entry.remove_css_class("error");
        let (h, s, v) = colors::rgb_to_hsv(c);
        // A grey has no hue and black has no saturation: keep what the user last chose.
        if s > 0.0 && v > 0.0 {
            self.scales[0].set_value(f64::from(h));
        }
        if v > 0.0 {
            self.scales[1].set_value(f64::from(s * 100.0));
        }
        self.scales[2].set_value(f64::from(v * 100.0));
        self.updating.set(false);
    }

    fn on_sliders(&self) {
        if self.updating.get() {
            return;
        }
        let [h, s, v] = &self.scales;
        let c = colors::hsv_to_rgb(
            h.value() as f32,
            s.value() as f32 / 100.0,
            v.value() as f32 / 100.0,
        );
        self.updating.set(true);
        self.entry.set_text(&c.to_string());
        self.entry.remove_css_class("error");
        self.updating.set(false);
        (self.on_change)(c);
    }

    fn on_entry(&self) {
        if self.updating.get() {
            return;
        }
        match parse_hex(&self.entry.text()) {
            Some(c) => {
                self.updating.set(true);
                let (h, s, v) = colors::rgb_to_hsv(c);
                if s > 0.0 && v > 0.0 {
                    self.scales[0].set_value(f64::from(h));
                }
                if v > 0.0 {
                    self.scales[1].set_value(f64::from(s * 100.0));
                }
                self.scales[2].set_value(f64::from(v * 100.0));
                self.updating.set(false);
                self.entry.remove_css_class("error");
                (self.on_change)(c);
            }
            None => self.entry.add_css_class("error"),
        }
    }
}

/// `#rrggbb` or `rrggbb`, any case.
pub fn parse_hex(text: &str) -> Option<Rgb> {
    let t = text.trim();
    let t = if t.starts_with('#') {
        t.to_owned()
    } else {
        format!("#{t}")
    };
    Rgb::from_str(&t).ok()
}

#[derive(Clone)]
pub struct Picker {
    pub root: gtk::Box,
    inner: Rc<Inner>,
}

impl Picker {
    /// Show `c` without reporting a change.
    pub fn set(&self, c: Rgb) {
        self.inner.show(c);
    }
}

pub fn new(initial: Rgb, on_change: impl Fn(Rgb) + 'static) -> Picker {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let swatches = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let mk = |max: f64| gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, max, 1.0);
    let scales = [mk(360.0), mk(100.0), mk(100.0)];
    let entry = gtk::Entry::builder()
        .max_length(7)
        .width_chars(8)
        .max_width_chars(8)
        .build();
    let inner = Rc::new(Inner {
        entry: entry.clone(),
        scales: scales.clone(),
        updating: Cell::new(false),
        on_change: Rc::new(on_change),
    });

    for c in PALETTE {
        let b = gtk::Button::new();
        b.add_css_class("swatch");
        b.add_css_class(&colors::class_name(c));
        b.set_tooltip_text(Some(&c.to_string()));
        let w = Rc::downgrade(&inner);
        b.connect_clicked(move |_| {
            if let Some(i) = w.upgrade() {
                i.show(c);
                (i.on_change)(c);
            }
        });
        swatches.append(&b);
    }
    root.append(&swatches);

    let grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(12)
        .build();
    for (row, (name, scale)) in ["Hue", "Saturation", "Brightness"]
        .iter()
        .zip(&scales)
        .enumerate()
    {
        let l = gtk::Label::new(Some(name));
        l.set_xalign(0.0);
        scale.set_hexpand(true);
        scale.set_draw_value(false);
        grid.attach(&l, 0, row as i32, 1, 1);
        grid.attach(scale, 1, row as i32, 1, 1);
        let w = Rc::downgrade(&inner);
        scale.connect_value_changed(move |_| {
            if let Some(i) = w.upgrade() {
                i.on_sliders();
            }
        });
    }
    let hex = gtk::Label::new(Some("Hex"));
    hex.set_xalign(0.0);
    grid.attach(&hex, 0, 3, 1, 1);
    entry.set_halign(gtk::Align::Start);
    grid.attach(&entry, 1, 3, 1, 1);
    let w = Rc::downgrade(&inner);
    entry.connect_changed(move |_| {
        if let Some(i) = w.upgrade() {
            i.on_entry();
        }
    });
    root.append(&grid);

    // The widgets only hold weak references to `inner`; the root keeps it alive.
    let keep = Rc::clone(&inner);
    root.connect_destroy(move |_| {
        let _ = &keep;
    });
    inner.show(initial);
    Picker { root, inner }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses_with_or_without_hash_and_rejects_junk() {
        assert_eq!(parse_hex("#ff8000"), Some(Rgb(255, 128, 0)));
        assert_eq!(parse_hex(" FF8000 "), Some(Rgb(255, 128, 0)));
        assert_eq!(parse_hex("#ff80"), None);
        assert_eq!(parse_hex("#gg0000"), None);
        assert_eq!(parse_hex(""), None);
    }
}
