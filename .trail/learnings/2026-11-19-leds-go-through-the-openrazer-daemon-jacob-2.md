---
title: "LEDs go through the OpenRazer daemon's D-Bus API, which is required, not sysfs"
author: Jacob
date: 2026-11-19
branch: main
cost: { minutes: 1, tokens: 5268 }
id: l-f9f7e251
scope: repo
status: active
replaces: l-c1f6fc0b
src: agent
touches:
  - CLAUDE.md
  - docs/hardware.md
---

## Decided
- openrazer-daemon is required; our daemon refuses to start without it. All LED and per-key lighting writes use its D-Bus API (org.razer), never sysfs, so two writers don't fight over the device.
- setKeyRow writes its payload verbatim in ONE write to the driver's custom frame file, so a full 4-row, 84-byte frame fits in a single D-Bus call. setCustom is called once to enter custom mode; after that, frames need no reset. Confirmed on hardware: two full frames switched with no blank flash, and the profile LEDs (B+G together, R alone) drive independently.
- At most 7 modes (3 unless a profile sets extended_modes). The mode LED mask is never 0, so an all-dark thumb module can't look like a dead device.

## Tried and ruled out
- Using sysfs as the runtime path: the live-update results were first proven there, but it can conflict with the running daemon.

## Watch out
- The device object path is the serial number (/org/razer/device/<serial>). Discover it at runtime; never hard-code it.
- Readback from the getters only proves the daemon accepted a write; visual confirmation on the pad is what verified the behavior.

In the code:
- Verified behavior is recorded in docs/hardware.md ("OpenRazer D-Bus path").
- Daemon source: openrazer_daemon/dbus_services/dbus_methods/chroma_keyboard.py (set_key_row), hardware/device_base.py (_set_key_row).
