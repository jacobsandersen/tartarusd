---
title: "Output uses two virtual devices, a keyboard and a pointer, so wheel and mouse buttons still work"
author: Jacob
date: 2026-11-19
branch: main
cost: { minutes: 3, tokens: 62180 }
id: l-27310e78
scope: repo
status: active
src: agent
touches:
  - packaging/70-tartarusd.rules
  - packaging/tartarusd.service
  - Cargo.lock
  - crates/core/src/keys.rs
  - crates/daemon/Cargo.toml
  - crates/daemon/src/device.rs
  - crates/daemon/src/emit.rs
  - crates/daemon/src/led.rs
  - crates/daemon/src/main.rs
  - crates/daemon/src/translate.rs
---

## Decided
- Remapped output goes to two uinput devices: a keyboard (every KEY_* code) and a pointer (BTN_LEFT..BTN_TASK, REL_X, REL_Y, REL_WHEEL, REL_WHEEL_HI_RES). Mouse-button chords and wheel passthrough route to the pointer.
- A wheel passthrough emits both REL_WHEEL (±1) and REL_WHEEL_HI_RES (±120). Input uses only REL_WHEEL and drops the hi-res event, since the pair is regenerated on output.

## Tried and ruled out
- A single virtual device with keys, buttons and only wheel axes: libinput generally treats a device as a pointer only when it has REL_X/REL_Y, so wheel and button output risks being ignored. This is from memory of libinput behavior and has NOT been tested on this machine yet.
- Putting non-mouse BTN_* codes (gamepad etc.) on the keyboard device: udev may classify the device as a joystick, so they are excluded; bindings to them are silently unsupported for now.

## Watch out
- If wheel or middle-click output ever fails to reach apps, check this split first: run evtest on the virtual pointer.
- The first live run on real hardware had not happened when this was written.

In the code:
- crates/daemon/src/emit.rs: Outputs::new, route, is_mouse_button.
- crates/daemon/src/translate.rs: wheel handling.
