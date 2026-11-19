# tartarusd

Rust workspace: userspace remapping daemon + CLI + GTK4 GUI for the Razer Tartarus V2
on Linux (any desktop environment). Restores Synapse-style keymaps: the hyper/mode key shifts into a per-mode
Hypershift layer without changing mode, and double-tapping it (or single-tapping, per
`mode_switch`) cycles up to 7 modes (profile LEDs show the mode). Each mode and Hypershift layer has its own
per-key lighting. Bindings are organized as per-game profiles, with optional
automatic switching by active window (Hyprland support is an opt-in cargo feature).
Replaces input-remapper for this device.

- Full design, data model, D-Bus interface, milestones: @DESIGN.md
- Verified hardware facts (codes, nodes, LEDs): @docs/hardware.md
- LED matrix probe: `tools/led_probe.py` (run with `sudo python3 -I tools/led_probe.py`)

## Status

Done: hardware discovery (0), `core` (1), daemon MVP (2), profile directory + live reload
(3), D-Bus + CLI (4), window-based switching behind the opt-in `hyprland` feature (5),
macros + Hypershift (6), per-key lighting (6b), GTK editor (7), input-remapper importer (8),
packaging + README (9). The mode key supports
two gestures (`mode_switch`, default `double_tap`).

All initial milestones are done. The GUI (`crates/gui`, plain gtk4-rs + libadwaita, not relm4)
is built and screenshot-checked, but its click-through behavior has not been exercised on a
real session end to end; verify before claiming it works. Pure editor logic (`edit`, `store`,
`text`, `layout`) is unit tested; widgets are not. Everything else has unit tests; most of it
has been run on the real device by the user, but verify before claiming hardware behavior
(see docs/hardware.md for what is confirmed).

Decision made: own daemon, NOT an xremap wrapper (rationale in DESIGN.md).

Decisions (2026-11-19):
- Profile LEDs are three independent LEDs (B=1, G=2, R=4). Modes 0–2 = B, G, R;
  modes 3+ light combinations. More than 3 modes is a per-profile opt-in
  (`extended_modes = true`), since cycling many modes mid-game is error-prone.
  Max 7 modes: at least one LED is always lit (all-off looks like a dead device).
- Wheel and D-pad are remappable (`PhysicalKey` covers them).
- Toolchain: nightly Rust, pinned via `rust-toolchain.toml`.
- OpenRazer is required (`openrazer-daemon` must be running; the daemon refuses to start
  without it). LEDs and per-key frames go through its D-Bus API (`org.razer`), never
  sysfs, so we don't fight it for the device.

## Ground rules

- No kernel code. LEDs go through openrazer-daemon's D-Bus API (required). The sysfs
  attrs in docs/hardware.md document verified device behavior, not the runtime path.
  The D-Bus frame and profile-LED paths are verified (see docs/hardware.md).
- Open input devices via `/dev/input/by-id/usb-Razer_Razer_Tartarus_V2-*` and sysfs via
  glob `0003:1532:022B.*`; instance/event numbers are not stable.
- Do not hard-code anything listed as unverified in docs/hardware.md.
- Desktop-independent by default: the daemon, CLI and GUI must work on any Linux DE or
  window manager. Compositor-specific code (Hyprland today) lives behind an opt-in
  cargo feature, off by default, and everything else must build and run without it.
- `crates/core` must stay free of I/O.
- Key-release invariant: release what was pressed (`held[code]`), never re-look-up
  the current mode's table on key-up.
- Mode key never emits output. Two gestures, chosen by `mode_switch`: `double_tap`
  (default: Hypershift on press, two quick taps cycle modes) or `tap` (a single tap cycles;
  Hypershift on the next input or after `tap_term`). Neither adds latency to other keys.
- Lighting writes never block the input path (separate task, latest-wins channel).
- Profile TOML files are the single source of truth; the GUI writes files atomically,
  the daemon reloads them.
- Arch packaging lives in `packaging/arch/PKGBUILD` (split packages, builds from the checkout).
- Daemon runs as a `systemd --user` service with udev `uaccess`; never require root.
- Check both feature configs: `cargo clippy --all-targets -- -D warnings` and
  `cargo clippy --all-targets --features tartarusd/hyprland,tartarusd-gui/hyprland -- -D warnings`.
- Best practices throughout: `cargo clippy -- -D warnings`, `cargo fmt`, unit tests
  for each milestone, `tracing` for logs, typed errors (`thiserror`) in libraries.
