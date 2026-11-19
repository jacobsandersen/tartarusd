//! Per-key lighting via OpenRazer's chroma interface, on its own task fed by a
//! latest-wins channel: the input path never waits on USB, and rapid mode or Hypershift
//! changes coalesce into the final frame.
//!
//! Verified behavior (docs/hardware.md): `setCustom` once enters custom mode, after
//! which each full frame written with `setKeyRow` shows immediately, with no reset
//! and no blanking in between.

use std::sync::Arc;

use anyhow::Context;
use tartarusd_core::Payload;
use tokio::sync::watch;
use tracing::{debug, error};
use zbus::Proxy;

use crate::razer::Razer;

const CHROMA_IFACE: &str = "razer.device.lighting.chroma";

/// The three D-Bus calls we use, abstracted so the call sequence is testable.
pub trait Chroma {
    async fn set_custom(&self) -> anyhow::Result<()>;
    async fn set_key_row(&self, frame: &[u8]) -> anyhow::Result<()>;
    /// Hand the pad back to the effect OpenRazer last set (its documented purpose:
    /// "applications that use custom matrix frames, after they exit").
    async fn restore_last_effect(&self) -> anyhow::Result<()>;
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

    async fn restore_last_effect(&self) -> anyhow::Result<()> {
        self.call::<_, _, ()>("restoreLastEffect", &())
            .await
            .context("restoreLastEffect failed")
    }
}

pub struct Lighting<C> {
    chroma: C,
    /// We switched the pad to custom mode and have not handed it back.
    custom: bool,
    last: Option<Arc<Payload>>,
}

impl<C: Chroma> Lighting<C> {
    pub fn new(chroma: C) -> Lighting<C> {
        Lighting {
            chroma,
            custom: false,
            last: None,
        }
    }

    /// Show `frame`, or hand the pad back on `None` (if we had taken it over).
    pub async fn apply(&mut self, frame: Option<&Arc<Payload>>) -> anyhow::Result<()> {
        match frame {
            Some(f) => {
                if self.last.as_ref().is_some_and(|l| **l == **f) {
                    return Ok(());
                }
                if !self.custom {
                    self.chroma.set_custom().await?;
                    self.custom = true;
                }
                self.chroma.set_key_row(f.as_slice()).await?;
                self.last = Some(f.clone());
            }
            None => {
                if self.custom {
                    self.chroma.restore_last_effect().await?;
                    self.custom = false;
                    self.last = None;
                }
            }
        }
        Ok(())
    }

    /// After an error, forget what we believe is on the pad so the next frame rewrites it.
    fn invalidate(&mut self) {
        self.custom = false;
        self.last = None;
    }
}

impl Lighting<Proxy<'static>> {
    pub async fn connect(razer: &Razer) -> zbus::Result<Self> {
        Ok(Lighting::new(razer.proxy(CHROMA_IFACE).await?))
    }
}

/// Apply the latest frame whenever it changes. Ends, after applying the final value
/// (send `None` to hand the pad back), when the sender is dropped.
pub async fn run<C: Chroma>(
    mut lighting: Lighting<C>,
    mut rx: watch::Receiver<Option<Arc<Payload>>>,
) {
    loop {
        let frame = rx.borrow_and_update().clone();
        match lighting.apply(frame.as_ref()).await {
            Ok(()) => debug!(lit = frame.is_some(), "lighting applied"),
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
    use std::{cell::RefCell, rc::Rc};

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
        async fn restore_last_effect(&self) -> anyhow::Result<()> {
            self.log("restore")
        }
    }

    /// A frame whose first color byte (offset 3) is `tag`.
    fn frame(tag: u8) -> Arc<Payload> {
        let mut p = [0u8; 84];
        p[3] = tag;
        Arc::new(p)
    }

    #[tokio::test]
    async fn enters_custom_mode_once_then_only_writes_frames() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        l.apply(Some(&frame(1))).await.unwrap();
        l.apply(Some(&frame(2))).await.unwrap();
        l.apply(Some(&frame(3))).await.unwrap();
        assert_eq!(f.take(), ["custom", "row:1", "row:2", "row:3"]);
    }

    #[tokio::test]
    async fn identical_frames_are_not_rewritten() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        l.apply(Some(&frame(1))).await.unwrap();
        l.apply(Some(&frame(1))).await.unwrap();
        assert_eq!(f.take(), ["custom", "row:1"]);
    }

    #[tokio::test]
    async fn hands_the_pad_back_only_if_it_was_taken() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        l.apply(None).await.unwrap();
        assert!(f.take().is_empty(), "never touched, nothing to restore");

        l.apply(Some(&frame(1))).await.unwrap();
        l.apply(None).await.unwrap();
        l.apply(None).await.unwrap();
        assert_eq!(f.take(), ["custom", "row:1", "restore"]);

        // Taking it over again re-enters custom mode.
        l.apply(Some(&frame(1))).await.unwrap();
        assert_eq!(f.take(), ["custom", "row:1"]);
    }

    #[tokio::test]
    async fn a_failed_write_is_retried_in_full() {
        let f = Fake::default();
        let mut l = Lighting::new(f.clone());
        l.apply(Some(&frame(1))).await.unwrap();
        f.take();
        *f.fail_next.borrow_mut() = true;
        assert!(l.apply(Some(&frame(2))).await.is_err());
        l.invalidate();
        l.apply(Some(&frame(2))).await.unwrap();
        assert_eq!(
            f.take(),
            ["custom", "row:2"],
            "re-enter custom mode and rewrite"
        );
    }

    #[tokio::test]
    async fn run_applies_latest_value_and_the_final_handback() {
        let f = Fake::default();
        let (tx, rx) = watch::channel(Some(frame(1)));
        tokio::join!(run(Lighting::new(f.clone()), rx), async move {
            tokio::task::yield_now().await;
            tx.send_replace(Some(frame(2)));
            tx.send_replace(None);
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
            Arc::new(
                tartarusd_core::Frame {
                    default: c,
                    ..Default::default()
                }
                .render(),
            )
        };
        let a = solid(tartarusd_core::Rgb(0, 255, 255));
        let b = solid(tartarusd_core::Rgb(255, 0, 0));
        l.apply(Some(&a))
            .await
            .expect("first frame (setCustom + setKeyRow)");
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        l.apply(Some(&b))
            .await
            .expect("second frame (setKeyRow only)");
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        l.apply(None).await.expect("hand back (restoreLastEffect)");
    }
}
