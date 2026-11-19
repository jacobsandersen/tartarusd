---
title: "LEDs go through the OpenRazer daemon's D-Bus API, which is required, not sysfs"
author: Jacob
date: 2026-11-19
branch: main
cost: { minutes: 19, tokens: 131422 }
id: l-c1f6fc0b
scope: repo
status: active
src: agent
touches:
  - crates/core/Cargo.toml
  - crates/core/src/action.rs
  - crates/core/src/config.rs
  - crates/core/src/keys.rs
  - crates/core/src/keys_table.rs
  - crates/core/src/lib.rs
  - crates/core/src/machine.rs
  - crates/core/src/profile.rs
  - tools/gen_keys.py
  - Cargo.lock
---

## Decided
- openrazer-daemon is required; our daemon refuses to start without it. All LED and per-key lighting writes use its D-Bus API (org.razer), never sysfs, so two writers don't fight over the device.
- OpenRazer's setKeyRow writes its payload verbatim in ONE write to the driver's custom frame file, so a full 4-row, 84-byte frame fits in a single D-Bus call. The one-time setCustom call enters custom mode; after that, frames need no reset.
- At most 7 modes (3 unless a profile sets extended_modes). The mode LED mask is never 0, so an all-dark thumb module can't look like a dead device.

## Tried and ruled out
- Using sysfs as the runtime path: the live-update results were proven there, but it can conflict with the running daemon.

## Watch out
- The device object path is the serial number (/org/razer/device/<serial>). Discover it at runtime; never hard-code it.
- The D-Bus calls returned success and the device was restored, but the visual result (no blank flash between frames) still needs the user's confirmation before docs/hardware.md calls it verified.

In the code:
- Probe script used: scratchpad dbus_frame_test.py (calls setCustom, setKeyRow x2, setGreenLED, setNone).
- Daemon source: openrazer_daemon/dbus_services/dbus_methods/chroma_keyboard.py (set_key_row), hardware/device_base.py (_set_key_row).
