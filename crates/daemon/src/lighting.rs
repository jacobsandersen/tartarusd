//! Per-key lighting via OpenRazer's chroma interface, on its own task fed by a
//! latest-wins channel: the input path never waits on USB, and rapid mode or Hypershift
//! changes coalesce into the final frame.
//!
//! Verified behavior (docs/hardware.md): `setCustom` once enters custom mode, after
//! which each full frame written with `setKeyRow` shows immediately, with no reset
//! and no blanking in between.

use anyhow::Context;
use tartarusd_core::{Pad, PadEffect, Rgb, Travel};
use tokio::sync::watch;
use tracing::{debug, error};
use zbus::Proxy;

use crate::razer::Razer;

const CHROMA_IFACE: &str = "razer.device.lighting.chroma";

/// OpenRazer's own record of the pad's effect, read before we replace it so it can be put
/// back. (Every firmware `set*` call overwrites what `restoreLastEffect` would restore.)
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub effect: String,
    pub colors: Vec<u8>,
    pub speed: i32,
    pub wave_dir: i32,
}

/// The D-Bus calls we use, abstracted so the call sequence is testable.
pub trait Chroma {
    async fn set_custom(&self) -> anyhow::Result<()>;
    async fn set_key_row(&self, frame: &[u8]) -> anyhow::Result<()>;
    async fn set_effect(&self, effect: &PadEffect) -> anyhow::Result<()>;
    /// What OpenRazer currently believes the effect is.
    async fn read_effect(&self) -> anyhow::Result<Snapshot>;
    /// Set the effect described by `snapshot` again. `Ok(false)` if it can't be expressed.
    async fn restore_effect(&self, snapshot: &Snapshot) -> anyhow::Result<bool>;
    /// Hand the pad back to the effect OpenRazer last set (its documented purpose:
    /// "applications that use custom matrix frames, after they exit").
    async fn restore_last_effect(&self) -> anyhow::Result<()>;
}

fn rgb(c: &Rgb) -> (u8, u8, u8) {
    (c.0, c.1, c.2)
}

impl Chroma for Proxy<'static> {
    async fn set_custom(&self) -> anyhow::Result<()> {
        self.call::<_, _, ()>("setCustom", &())
            .await
            .context("setCustom failed")
    }

    async fn set_key_row(&self, frame: &[u8]) -> anyhow::Result<()> {
        self.call::<_, _, ()>("setKeyRow", &(frame,))
            .await
            .context("setKeyRow failed")
    }

    // OpenRazer takes colors first and the speed last (confirmed in its source).
    async fn set_effect(&self, effect: &PadEffect) -> anyhow::Result<()> {
        let (name, result) = match effect {
            PadEffect::Spectrum => (
                "setSpectrum",
                self.call::<_, _, ()>("setSpectrum", &()).await,
            ),
            PadEffect::Wave(dir) => {
                // Confirmed on the device: 1 travels right to left, 2 left to right.
                let d: i32 = if *dir == Travel::Left { 1 } else { 2 };
                ("setWave", self.call("setWave", &(d,)).await)
            }
            PadEffect::Breathing(c) => match c.as_slice() {
                [] => ("setBreathRandom", self.call("setBreathRandom", &()).await),
                [a] => (
                    "setBreathSingle",
                    self.call("setBreathSingle", &rgb(a)).await,
                ),
                [a, b, ..] => (
                    "setBreathDual",
                    self.call("setBreathDual", &(rgb(a), rgb(b))).await,
                ),
            },
            PadEffect::Reactive { color, speed } => {
                let (r, g, b) = rgb(color);
                (
                    "setReactive",
                    self.call("setReactive", &(r, g, b, *speed)).await,
                )
            }
            PadEffect::Starlight { colors, speed } => match colors.as_slice() {
                [] => (
                    "setStarlightRandom",
                    self.call("setStarlightRandom", &(*speed,)).await,
                ),
                [a] => {
                    let (r, g, b) = rgb(a);
                    (
                        "setStarlightSingle",
                        self.call("setStarlightSingle", &(r, g, b, *speed)).await,
                    )
                }
                [a, c, ..] => {
                    let ((r1, g1, b1), (r2, g2, b2)) = (rgb(a), rgb(c));
                    (
                        "setStarlightDual",
                        self.call("setStarlightDual", &(r1, g1, b1, r2, g2, b2, *speed))
                            .await,
                    )
                }
            },
        };
        result.with_context(|| format!("{name} failed"))
    }

    async fn read_effect(&self) -> anyhow::Result<Snapshot> {
        Ok(Snapshot {
            effect: self
                .call("getEffect", &())
                .await
                .context("getEffect failed")?,
            colors: self
                .call("getEffectColors", &())
                .await
                .context("getEffectColors failed")?,
            speed: self
                .call("getEffectSpeed", &())
                .await
                .context("getEffectSpeed failed")?,
            wave_dir: self
                .call("getWaveDir", &())
                .await
                .context("getWaveDir failed")?,
        })
    }

    async fn restore_effect(&self, s: &Snapshot) -> anyhow::Result<bool> {
        let c = |i: usize| s.colors.get(i).copied().unwrap_or(0);
        let speed = u8::try_from(s.speed).unwrap_or(2);
        let r = match s.effect.as_str() {
            "none" => self.call("setNone", &()).await,
            "static" => self.call("setStatic", &(c(0), c(1), c(2))).await,
            "spectrum" => self.call("setSpectrum", &()).await,
            "wave" => self.call("setWave", &(s.wave_dir,)).await,
            "reactive" => self.call("setReactive", &(c(0), c(1), c(2), speed)).await,
            "breathSingle" => self.call("setBreathSingle", &(c(0), c(1), c(2))).await,
            "breathDual" => {
                self.call("setBreathDual", &(c(0), c(1), c(2), c(3), c(4), c(5)))
                    .await
            }
            "breathRandom" => self.call("setBreathRandom", &()).await,
            "starlightSingle" => {
                self.call("setStarlightSingle", &(c(0), c(1), c(2), speed))
                    .await
            }
            "starlightDual" => {
                self.call(
                    "setStarlightDual",
                    &(c(0), c(1), c(2), c(3), c(4), c(5), speed),
                )
                .await
            }
            "starlightRandom" => self.call("setStarlightRandom", &(speed,)).await,
            _ => return Ok(false),
        };
        r.map(|()| true)
            .context("restoring the previous effect failed")
    }

    async fn restore_last_effect(&self) -> anyhow::Result<()> {
        self.call::<_, _, ()>("restoreLastEffect", &())
            .await
            .context("restoreLastEffect failed")
    }
}

pub struct Lighting<C> {
    chroma: C,
    /// We changed the pad (custom frame or effect) and have not handed it back.
    taken: bool,
    /// The pad is in custom mode, so frames can be written without `setCustom`.
    in_custom: bool,
    /// The effect that was configured before we first used a firmware effect.
    snapshot: Option<Snapshot>,
    last: Option<Pad>,
}

impl<C: Chroma> Lighting<C> {
    pub fn new(chroma: C) -> Lighting<C> {
        Lighting {
            chroma,
            taken: false,
            in_custom: false,
            snapshot: None,
            last: None,
        }
    }

    /// Show `pad`, or hand the pad back on [`Pad::Handback`] (if we had taken it over).
    pub async fn apply(&mut self, pad: &Pad) -> anyhow::Result<()> {
        match pad {
            Pad::Frame(f) => {
                if self.last.as_ref() == Some(pad) {
                    return Ok(());
                }
                if !self.in_custom {
                    self.chroma.set_custom().await?;
                    self.in_custom = true;
                }
                self.taken = true;
                self.chroma.set_key_row(f.as_slice()).await?;
                self.last = Some(pad.clone());
            }
            Pad::Effect(e) => {
                if self.last.as_ref() == Some(pad) {
                    return Ok(());
                }
                // Firmware effects overwrite OpenRazer's remembered effect; note it first.
                if self.snapshot.is_none() {
                    self.snapshot = Some(self.chroma.read_effect().await?);
                }
                self.taken = true;
                self.chroma.set_effect(e).await?;
                self.in_custom = false;
                self.last = Some(pad.clone());
            }
            Pad::Handback => {
                if self.taken {
                    let restored = match &self.snapshot {
                        Some(s) => self.chroma.restore_effect(s).await?,
                        None => false,
                    };
                    if !restored {
                        self.chroma.restore_last_effect().await?;
                    }
                    self.snapshot = None;
                    self.taken = false;
                    self.in_custom = false;
                    self.last = None;
                }
            }
        }
        Ok(())
    }

    /// After an error, forget what we believe is on the pad so the next write redoes it in
    /// full. What to give back later is kept.
    fn invalidate(&mut self) {
        self.in_custom = false;
        self.last = None;
    }
}

impl Lighting<Proxy<'static>> {
    pub async fn connect(razer: &Razer) -> zbus::Result<Self> {
        Ok(Lighting::new(razer.proxy(CHROMA_IFACE).await?))
    }
}

/// Apply the latest pad state whenever it changes. Ends, after applying the final value
/// (send [`Pad::Handback`] to hand the pad back), when the sender is dropped.
pub async fn run<C: Chroma>(mut lighting: Lighting<C>, mut rx: watch::Receiver<Pad>) {
    loop {
        let pad = rx.borrow_and_update().clone();
        match lighting.apply(&pad).await {
            Ok(()) => debug!(pad = ?std::mem::discriminant(&pad), "lighting applied"),
            Err(e) => {
                error!("lighting update failed: {e:#}");
                lighting.invalidate();
            }
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    use tartarusd_core::Payload;

    use super::*;

    #[derive(Clone, Default)]
    struct Fake {
        calls: Rc<RefCell<Vec<String>>>,
        fail_next: Rc<RefCell<bool>>,
    }

    impl Fake {
        fn log(&self, s: &str) -> anyhow::Result<()> {
            if self.fail_next.replace(false) {
                anyhow::bail!("injected failure");
            }
            self.calls.borrow_mut().push(s.to_owned());
            Ok(())
        }
        fn take(&self) -> Vec<String> {
            self.calls.borrow_mut().drain(..).collect()
        }
    }

    impl Chroma for Fake {
        async fn set_custom(&self) -> anyhow::Result<()> {
            self.log("custom")
        }
        async fn set_key_row(&self, frame: &[u8]) -> anyhow::Result<()> {
            self.log(&format!("row:{}", frame[3]))
        }
        async fn set_effect(&self, e: &PadEffect) -> anyhow::Result<()> {
            self.log(&format!(
                "effect:{}",
                e.to_table()["type"].as_str().unwrap()
            ))
        }
        async fn read_effect(&self) -> anyhow::Result<Snapshot> {
            self.log("snapshot")?;
            Ok(Snapshot {
                effect: "static".into(),
                colors: vec![1, 2, 3],
                speed: 2,
                wave_dir: 1,
            })
        }
        async fn restore_effect(&self, s: &Snapshot) -> anyhow::Result<bool> {
            self.log(&format!("restore:{}", s.effect))?;
            Ok(true)
        }
        async fn restore_last_effect(&self) -> anyhow::Result<()> {
            self.log("restore")
        }
    }

    /// A frame whose first color byte (offset 3) is `tag`.
    fn frame(tag: u8) -> Pad {
        let mut p: Payload = [0u8; 84];
        p[3] = tag;
        Pad::Frame(Arc::new(p))
    }

    fn effect(e: PadEffect) -> Pad {
        Pad::Effect(e)
    }

    #[tokio::test]
    async fn enters_custom_mode_once_then_only_writes_frames() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        l.apply(&frame(1)).await.unwrap();
        l.apply(&frame(2)).await.unwrap();
        l.apply(&frame(3)).await.unwrap();
        assert_eq!(f.take(), ["custom", "row:1", "row:2", "row:3"]);
    }

    #[tokio::test]
    async fn identical_frames_are_not_rewritten() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        l.apply(&frame(1)).await.unwrap();
        l.apply(&frame(1)).await.unwrap();
        assert_eq!(f.take(), ["custom", "row:1"]);
    }

    #[tokio::test]
    async fn hands_the_pad_back_only_if_it_was_taken() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        l.apply(&Pad::Handback).await.unwrap();
        assert!(f.take().is_empty(), "never touched, nothing to restore");

        l.apply(&frame(1)).await.unwrap();
        l.apply(&Pad::Handback).await.unwrap();
        l.apply(&Pad::Handback).await.unwrap();
        assert_eq!(
            f.take(),
            ["custom", "row:1", "restore"],
            "frames don't change OpenRazer's remembered effect, so it can restore it"
        );

        // Taking it over again re-enters custom mode.
        l.apply(&frame(1)).await.unwrap();
        assert_eq!(f.take(), ["custom", "row:1"]);
    }

    #[tokio::test]
    async fn a_failed_write_is_retried_in_full() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        l.apply(&frame(1)).await.unwrap();
        f.take();
        *f.fail_next.borrow_mut() = true;
        assert!(l.apply(&frame(2)).await.is_err());
        l.invalidate();
        l.apply(&frame(2)).await.unwrap();
        assert_eq!(
            f.take(),
            ["custom", "row:2"],
            "re-enter custom mode and rewrite"
        );
    }

    #[tokio::test]
    async fn effects_remember_the_configured_effect_once_and_put_it_back() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        let wave = effect(PadEffect::Wave(Travel::Left));
        l.apply(&wave).await.unwrap();
        l.apply(&wave).await.unwrap();
        assert_eq!(
            f.take(),
            ["snapshot", "effect:wave"],
            "noted once, not rewritten"
        );

        l.apply(&effect(PadEffect::Spectrum)).await.unwrap();
        l.apply(&frame(7)).await.unwrap();
        l.apply(&wave).await.unwrap();
        assert_eq!(
            f.take(),
            ["effect:spectrum", "custom", "row:7", "effect:wave"],
            "no second snapshot, and custom mode is re-entered after an effect"
        );

        l.apply(&Pad::Handback).await.unwrap();
        l.apply(&Pad::Handback).await.unwrap();
        assert_eq!(
            f.take(),
            ["restore:static"],
            "the original effect, not ours"
        );

        // A later session notes the configured effect afresh.
        l.apply(&wave).await.unwrap();
        assert_eq!(f.take(), ["snapshot", "effect:wave"]);
    }

    #[tokio::test]
    async fn a_failed_effect_keeps_what_to_give_back() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        l.apply(&effect(PadEffect::Spectrum)).await.unwrap();
        f.take();
        *f.fail_next.borrow_mut() = true;
        assert!(
            l.apply(&effect(PadEffect::Wave(Travel::Right)))
                .await
                .is_err()
        );
        l.invalidate();
        l.apply(&Pad::Handback).await.unwrap();
        assert_eq!(f.take(), ["restore:static"]);
    }

    #[tokio::test]
    async fn run_applies_latest_value_and_the_final_handback() {
        let f = Fake::default();
        let (tx, rx) = watch::channel(frame(1));
        tokio::join!(run(Lighting::new(f.clone()), rx), async move {
            tokio::task::yield_now().await;
            tx.send_replace(frame(2));
            tx.send_replace(Pad::Handback);
            // Dropping the sender ends the task after it applies the final value.
        });
        let calls = f.take();
        assert_eq!(calls.first().map(String::as_str), Some("custom"));
        assert_eq!(calls.last().map(String::as_str), Some("restore"));
    }

    /// Needs openrazer-daemon and the pad; writes one frame for a second, then hands the
    /// pad back. Run with `cargo test -p tartarusd live -- --ignored`.
    #[tokio::test]
    #[ignore = "requires openrazer-daemon and a Tartarus V2"]
    async fn live_frame_round_trip() {
        let razer = Razer::connect().await.expect("openrazer-daemon / device");
        let mut l = Lighting::connect(&razer).await.unwrap();
        let solid = |c| {
            Pad::Frame(Arc::new(
                tartarusd_core::Frame {
                    default: c,
                    ..Default::default()
                }
                .render(),
            ))
        };
        let a = solid(tartarusd_core::Rgb(0, 255, 255));
        let b = solid(tartarusd_core::Rgb(255, 0, 0));
        l.apply(&a)
            .await
            .expect("first frame (setCustom + setKeyRow)");
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        l.apply(&b).await.expect("second frame (setKeyRow only)");
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        l.apply(&Pad::Handback).await.expect("restoreLastEffect");
    }
}
