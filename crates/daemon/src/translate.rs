//! Raw evdev events -> state-machine events (pure; no I/O).

use evdev::{EventType, InputEvent, RelativeAxisCode};
use tartarusd_core::{Event, PhysicalKey, WheelDir};

/// Translate one raw event. Most raw events (SYN, MSC_SCAN, ...) yield nothing.
///
/// - Key auto-repeat (value 2) is dropped.
/// - A wheel notch arrives as `REL_WHEEL` plus `REL_WHEEL_HI_RES`; only `REL_WHEEL`
///   is used (the pair is regenerated on passthrough), and it becomes a press+release.
pub fn translate(ev: &InputEvent) -> Vec<Event> {
    let t = ev.event_type();
    if t == EventType::KEY {
        let key = PhysicalKey::Key(tartarusd_core::Key(ev.code()));
        match ev.value() {
            1 => vec![Event::Press(key)],
            0 => vec![Event::Release(key)],
            _ => vec![],
        }
    } else if t == EventType::RELATIVE && ev.code() == RelativeAxisCode::REL_WHEEL.0 {
        let dir = match ev.value() {
            v if v > 0 => WheelDir::Up,
            v if v < 0 => WheelDir::Down,
            _ => return vec![],
        };
        let k = PhysicalKey::Wheel(dir);
        vec![Event::Press(k), Event::Release(k)]
    } else {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: u16, value: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, code, value)
    }

    #[test]
    fn keys() {
        let k = PhysicalKey::Key(tartarusd_core::Key(2));
        assert_eq!(translate(&key(2, 1)), vec![Event::Press(k)]);
        assert_eq!(translate(&key(2, 0)), vec![Event::Release(k)]);
        assert!(translate(&key(2, 2)).is_empty(), "auto-repeat dropped");
    }

    #[test]
    fn wheel_uses_rel_wheel_only() {
        let rel = |code: RelativeAxisCode, v| InputEvent::new(EventType::RELATIVE.0, code.0, v);
        let up = PhysicalKey::Wheel(WheelDir::Up);
        assert_eq!(
            translate(&rel(RelativeAxisCode::REL_WHEEL, 1)),
            vec![Event::Press(up), Event::Release(up)]
        );
        assert_eq!(
            translate(&rel(RelativeAxisCode::REL_WHEEL, -1)),
            vec![
                Event::Press(PhysicalKey::Wheel(WheelDir::Down)),
                Event::Release(PhysicalKey::Wheel(WheelDir::Down))
            ]
        );
        assert!(translate(&rel(RelativeAxisCode::REL_WHEEL_HI_RES, 120)).is_empty());
        assert!(translate(&rel(RelativeAxisCode::REL_X, 5)).is_empty());
    }

    #[test]
    fn other_event_types_ignored() {
        assert!(translate(&InputEvent::new(EventType::MISC.0, 4, 458_792)).is_empty());
        assert!(translate(&InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)).is_empty());
    }
}
