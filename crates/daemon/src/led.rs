//! Profile LEDs (the three thumb-module LEDs) via the OpenRazer daemon's D-Bus API.

use anyhow::Context;
use tartarusd_core::profile::{LED_BLUE, LED_GREEN, LED_RED};
use tokio::sync::watch;
use tracing::error;
use zbus::Proxy;

use crate::razer::Razer;

const LED_IFACE: &str = "razer.device.lighting.profile_led";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Led {
    Blue,
    Green,
    Red,
}

impl Led {
    const ALL: [Led; 3] = [Led::Blue, Led::Green, Led::Red];

    fn bit(self) -> u8 {
        match self {
            Led::Blue => LED_BLUE,
            Led::Green => LED_GREEN,
            Led::Red => LED_RED,
        }
    }

    fn setter(self) -> &'static str {
        match self {
            Led::Blue => "setBlueLED",
            Led::Green => "setGreenLED",
            Led::Red => "setRedLED",
        }
    }
}

/// Which LED writes are needed to go from `prev` to `new`. LEDs turning on come first,
/// so the module is never fully dark mid-transition. `prev = None` writes all three.
pub fn plan(prev: Option<u8>, new: u8) -> Vec<(Led, bool)> {
    let changed = |l: Led| prev.is_none_or(|p| (p ^ new) & l.bit() != 0);
    let on = |l: Led| new & l.bit() != 0;
    let mut writes: Vec<(Led, bool)> = Led::ALL
        .into_iter()
        .filter(|&l| changed(l))
        .map(|l| (l, on(l)))
        .collect();
    writes.sort_by_key(|&(_, on)| !on); // stable: ons first
    writes
}

pub struct RazerLeds {
    proxy: Proxy<'static>,
    last: Option<u8>,
}

impl RazerLeds {
    pub async fn new(razer: &Razer) -> zbus::Result<RazerLeds> {
        Ok(RazerLeds {
            proxy: razer.proxy(LED_IFACE).await?,
            last: None,
        })
    }

    pub async fn apply(&mut self, mask: u8) -> anyhow::Result<()> {
        for (led, on) in plan(self.last, mask) {
            self.proxy
                .call::<_, _, ()>(led.setter(), &(on,))
                .await
                .with_context(|| format!("{} failed", led.setter()))?;
        }
        self.last = Some(mask);
        Ok(())
    }
}

/// Apply the latest requested mask whenever it changes. Writes never block the input
/// path: the caller only does `watch::Sender::send_replace`, and intermediate masks
/// coalesce. Ends (after applying the final value) when the sender is dropped.
pub async fn run(mut leds: RazerLeds, mut rx: watch::Receiver<u8>) {
    loop {
        let mask = *rx.borrow_and_update();
        if let Err(e) = leds.apply(mask).await {
            error!("LED update failed: {e:#}");
            // Force a full rewrite on the next change.
            leds.last = None;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_write_sets_all_three() {
        assert_eq!(
            plan(None, LED_BLUE),
            vec![(Led::Blue, true), (Led::Green, false), (Led::Red, false)]
        );
    }

    #[test]
    fn only_changed_leds_with_ons_first() {
        assert_eq!(
            plan(Some(LED_BLUE), LED_RED),
            vec![(Led::Red, true), (Led::Blue, false)]
        );
        assert_eq!(plan(Some(LED_BLUE), LED_BLUE), vec![]);
        assert_eq!(
            plan(Some(7), 1),
            vec![(Led::Green, false), (Led::Red, false)]
        );
    }
}
