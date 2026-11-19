//! State-machine effects -> uinput events. Two virtual devices: a keyboard, and a
//! pointer (mouse buttons + wheel) so libinput treats wheel/button output as a pointer.

use std::io;

use evdev::{
    AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode, uinput::VirtualDevice,
};
use tartarusd_core::{Edge, Effect, Key, WheelDir};

const BTN_LEFT: u16 = 0x110;
const BTN_TASK: u16 = 0x117;

fn is_mouse_button(code: u16) -> bool {
    (BTN_LEFT..=BTN_TASK).contains(&code)
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Batches {
    pub kbd: Vec<InputEvent>,
    pub mouse: Vec<InputEvent>,
}

/// Split effects into per-device event batches. LED, mode and shift effects are not
/// output events and are ignored here.
pub fn route(effects: &[Effect]) -> Batches {
    let mut b = Batches::default();
    for e in effects {
        match e {
            Effect::Key { key, edge } => {
                let ev = InputEvent::new(
                    EventType::KEY.0,
                    key.0,
                    i32::from(matches!(edge, Edge::Down)),
                );
                if is_mouse_button(key.0) {
                    b.mouse.push(ev);
                } else {
                    b.kbd.push(ev);
                }
            }
            Effect::Wheel(dir) => {
                let sign = if matches!(dir, WheelDir::Up) { 1 } else { -1 };
                b.mouse.push(InputEvent::new(
                    EventType::RELATIVE.0,
                    RelativeAxisCode::REL_WHEEL.0,
                    sign,
                ));
                b.mouse.push(InputEvent::new(
                    EventType::RELATIVE.0,
                    RelativeAxisCode::REL_WHEEL_HI_RES.0,
                    sign * 120,
                ));
            }
            Effect::SetLed(_)
            | Effect::SetFrame(_)
            | Effect::ModeChanged(_)
            | Effect::ShiftChanged(_) => {}
        }
    }
    b
}

pub struct Outputs {
    kbd: VirtualDevice,
    mouse: VirtualDevice,
}

impl Outputs {
    pub fn new() -> io::Result<Outputs> {
        // Keyboard: every KEY_* name. (Gamepad/other BTN_* are deliberately excluded so
        // udev doesn't classify the device as a joystick.)
        let mut keys = AttributeSet::<KeyCode>::new();
        for (Key(code), name) in Key::all_named() {
            if name.starts_with("KEY_") {
                keys.insert(KeyCode::new(code));
            }
        }
        let kbd = VirtualDevice::builder()?
            .name("tartarusd virtual keyboard")
            .with_keys(&keys)?
            .build()?;

        let mut buttons = AttributeSet::<KeyCode>::new();
        for code in BTN_LEFT..=BTN_TASK {
            buttons.insert(KeyCode::new(code));
        }
        let mut axes = AttributeSet::<RelativeAxisCode>::new();
        for a in [
            RelativeAxisCode::REL_X,
            RelativeAxisCode::REL_Y,
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_WHEEL_HI_RES,
        ] {
            axes.insert(a);
        }
        let mouse = VirtualDevice::builder()?
            .name("tartarusd virtual pointer")
            .with_keys(&buttons)?
            .with_relative_axes(&axes)?
            .build()?;
        Ok(Outputs { kbd, mouse })
    }

    pub fn emit(&mut self, effects: &[Effect]) -> io::Result<()> {
        let b = route(effects);
        if !b.kbd.is_empty() {
            self.kbd.emit(&b.kbd)?;
        }
        if !b.mouse.is_empty() {
            self.mouse.emit(&b.mouse)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(name: &str) -> Key {
        Key::from_name(name).unwrap()
    }

    #[test]
    fn keys_route_by_device() {
        let b = route(&[
            Effect::Key {
                key: k("KEY_A"),
                edge: Edge::Down,
            },
            Effect::Key {
                key: k("BTN_MIDDLE"),
                edge: Edge::Down,
            },
            Effect::Key {
                key: k("KEY_A"),
                edge: Edge::Up,
            },
            Effect::SetLed(1),
            Effect::ModeChanged(0),
        ]);
        assert_eq!(b.kbd.len(), 2);
        assert_eq!(b.kbd[0].value(), 1);
        assert_eq!(b.kbd[1].value(), 0);
        assert_eq!(b.mouse.len(), 1);
        assert_eq!(b.mouse[0].code(), k("BTN_MIDDLE").0);
    }

    #[test]
    fn wheel_emits_both_rel_events() {
        let b = route(&[Effect::Wheel(WheelDir::Down)]);
        assert!(b.kbd.is_empty());
        assert_eq!(
            b.mouse
                .iter()
                .map(|e| (e.code(), e.value()))
                .collect::<Vec<_>>(),
            vec![
                (RelativeAxisCode::REL_WHEEL.0, -1),
                (RelativeAxisCode::REL_WHEEL_HI_RES.0, -120)
            ]
        );
    }

    #[test]
    fn mouse_button_range() {
        assert!(is_mouse_button(k("BTN_LEFT").0));
        assert!(is_mouse_button(k("BTN_MIDDLE").0));
        assert!(!is_mouse_button(k("KEY_A").0));
    }
}
