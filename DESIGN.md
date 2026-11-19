# tartarusd — Design

Userspace remapping daemon + GUI for the Razer Tartarus V2 on Linux, restoring the
Windows/Synapse "keymap" feature: the hyper/mode key cycles up to 7 keymaps (tap) or
shifts into a per-mode Hypershift layer (hold), with mode LEDs and per-key lighting,
organized into per-game profiles.

## Background / premises

- On Windows, the 3 modes are **Synapse software keymaps** (Synapse supports up to 8
  per profile), not firmware state. The device sends ordinary key events; Synapse
  remaps them and sets the thumb-module LED over USB. On Linux nothing holds the
  mode state, so the device stays at power-on defaults (mode 1, blue LED).
- Therefore: **no kernel driver.** OpenRazer (`razerkbd`) stays bound for lighting;
  this project is an evdev → state machine → uinput daemon in userspace.
- Core model:

  ```
  mode ∈ ℤ/nℤ   (n = modes in the active profile, 1..=7)
  mode_key:  mode ← (mode + 1) mod n; set_led(mode)
  key k:     emit table[mode][k]   (passthrough if unmapped)
  ```

- This **replaces input-remapper** for the Tartarus. Both use exclusive `EVIOCGRAB`;
  only one grabber can hold the device. Disable input-remapper autoload for it.

## Decision: own daemon, not an xremap wrapper

Decided 2026-11-19. Wrapping xremap fails on: tap/hold mode key with mode-preserving
Hypershift, LED state synced to mode (xremap exposes no mode state), per-key lighting
per mode/Hypershift with ordered, coalesced writes, per-profile mode memory, GUI key
capture. Reused instead: OpenRazer kernel driver (LEDs), `evdev`, `zbus`, and (opt-in) Hyprland IPC.
xremap is reference reading only (check its license before borrowing code).

## Prior art (surveyed 2026-11-18)

No existing project combines: Tartarus V2 + mode key cycling keymaps with LED
feedback + per-game profiles + automatic window-based switching + GUI.

| Project | Has | Missing |
|---|---|---|
| xremap (Rust) | modes (`set_mode`), device filter, Hyprland app filter, `launch` commands | GUI, per-profile mode memory |
| OpenTartarus (Python/PyQt6) | evdev/uinput remap, profiles, RGB, tray | layers/modes, auto-switch, Wayland focus; V2 untested |
| keyzer (Python) | Synapse-style UI over input-remapper (Tartarus Pro, Naga Pro) | inherits input-remapper's lack of layers |
| aethermap (Rust + iced) | privileged daemon + GUI, macros, Tartarus support | layers/modes not documented |
| firemap | profile-based evdev/uinput, targets Tartarus V2 | modes, LED, auto-switch, GUI; early-stage |
| lintartarus | `LAYOUT_NEXT` layout switching, exe matching | original Tartarus era; V2/Wayland unknown |

Fallback if this project stalls: xremap config (modmap mode key → spare key,
keymap per mode with `set_mode` + `launch` for LED). Read xremap's mode handling
for reference when implementing `core`.

## Hardware facts — VERIFY FIRST (Milestone 0)

| Item | Current belief | How to verify |
|---|---|---|
| USB VID:PID | **VERIFIED** `1532:022B` | — |
| HID interfaces | **VERIFIED** 3 (`0003:1532:022B.000{3,4,5}`); LED attrs only on one | glob `.../0003:1532:022B.*/profile_led_blue` (instance number varies) |
| Profile LEDs | **VERIFIED** `profile_led_{red,green,blue}` sysfs attrs; independent bits, all may be on → 8 states (mask: R=4, G=2, B=1) | — |
| Mode/hyper key code | **VERIFIED** `KEY_LEFTALT` on if01 | — |
| Event nodes | **VERIFIED** 3: if00 kbd (silent), if01 kbd (all keys + D-pad), if02 mouse (wheel, `BTN_MIDDLE`) | see `docs/hardware.md` |
| Default key codes | **VERIFIED** 01–05 → `KEY_1..KEY_5`, 06–10 → Tab,Q,W,E,R, 11–15 → Caps,A,S,D,F, 16–19 → LShift,Z,X,C, 20 → Space | see `docs/hardware.md` |
| input-remapper preset schema | JSON under `~/.config/input-remapper-2/presets/<device>/<preset>.json` | Inspect real preset files |

Record verified results in `docs/hardware.md` and update this table.

## Architecture

```
            ┌──────── ~/.config/tartarusd/profiles/*.toml ────────┐
            │        (source of truth, hand-editable, git-able)   │
            ▼ inotify reload                          write ▲     │
┌──────────────────────────┐  D-Bus (session)  ┌─────────────┐    │
│ tartarusd (user service) │◄─────────────────►│ GUI (GTK4)  │────┘
│  evdev grab → state →    │◄─────────────────►│ CLI         │
│  uinput out              │◄──── signals ────►│ bar widget  │ (mode indicator)
│  LED via OpenRazer D-Bus │                   └─────────────┘
│  window source (opt-in)  │◄── active-window events (auto profile switch)
└──────────────────────────┘
```

Rules:
- The daemon is the **only** component that touches `/dev/input`, `/dev/uinput`, or LEDs.
- Profile files are the **only** persistent state. The GUI edits files; it never
  pushes bindings over IPC.
- The GUI saves atomically (write temp file + `rename(2)`); the daemon reloads on
  change (`notify` crate) and validates before swapping.

## Data model (`crates/core`)

```
Profile { name, match_rules: Vec<MatchRule>, modes: Vec<Mode> /* 1..=3, or 1..=7 if extended_modes */, extended_modes: bool, remember_mode: bool }
Mode    { led_mask: u8 /* 1..=7, R=4 G=2 B=1 */, bindings: Map<PhysicalKey, Action>,
          hypershift: Option<Map<PhysicalKey, Action>> }
Action  = Passthrough | Disabled | Chord(Vec<Key>)
        | Macro(Vec<Step { key, edge: Down|Up, delay_ms }>)
        | SetMode(u8) | NextMode
MatchRule { class: Option<String>, title_regex: Option<String> }
```

Example profile (`~/.config/tartarusd/profiles/elden-ring.toml`):

```toml
name = "Elden Ring"
remember_mode = true

[[match]]
class = "steam_app_1245620"   # window class / app id of Proton games

[mode.0]
led_mask = 1   # blue
KEY_1 = "KEY_R"
KEY_Q = { macro = [["KEY_E", 0], ["KEY_F", 50]] }

[mode.1]
led_mask = 2   # green

[mode.2]
led_mask = 4   # red
```

Global config (`~/.config/tartarusd/config.toml`): device match, `mode_key`,
`tap_term_ms`, `hypershift_fallthrough`, optional `hypershift_led_mask`, default profile name.

## Daemon (`crates/daemon`)

- **Event loop:** read all Tartarus event nodes; on each event:
  - mode key → tap/hold state machine (see "Mode key" section); never forwarded.
  - key down → `action = table[mode][code]` (fallback passthrough); emit; record
    `held[code] = action`.
  - key up → release `held.remove(code)`, **not** `table[mode][code]`. This prevents
    stuck keys when the mode changes while a key is held.
  - Emit `SYN_REPORT` after each output batch.
- **Profile swap:** build new tables → release everything in `held` → atomic swap
  (`arc-swap`).
- **Active profile precedence:** manual pin > auto-match > default.
- **Auto-switch (opt-in, compositor-specific):** the daemon core is desktop-independent
  and works on any DE/WM. Automatic switching needs to know the active window, which
  is compositor-specific, so it sits behind a small `window` module: a backend is a
  task that sends `ActiveWindow { class, title }` on every focus change (and once at
  startup). Each backend is an opt-in cargo
  feature, **off by default**; with none enabled, profile selection is manual pin >
  default and `[[match]]` rules are parsed and kept but unused (profiles stay portable).
  - Feature `hyprland` (`cargo build --features hyprland`): connects to Hyprland's event
    socket (`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`) for
    `activewindow>>class,title` events and asks the request socket (`.socket.sock`,
    `j/activewindow`) for the window already focused at startup. If the environment
    variable is missing it uses the newest instance under `$XDG_RUNTIME_DIR/hypr`. It
    reconnects if the compositor restarts.
  - Other backends (X11, KDE, GNOME, sway, ...) can be added the same way later;
    none are planned yet.
- **Matching:** a `[[match]]` rule needs `class` and/or `title_regex`; every field set
  must match (`class` is compared exactly and case-sensitively; `title_regex` is
  searched anywhere in the title). A profile matches if any of its rules does. Empty
  rules and invalid regexes are rejected at load. If several profiles match, the first
  by file name wins. A profile with no rules never auto-matches.
- **Selection** is re-evaluated on focus change, profile reload, pin and unpin:
  pinned profile > first matching profile > startup default. Switching releases held
  keys. If the selected profile disappears, selection falls back down that chain, and
  if nothing remains the loaded profile keeps running.
- **Mode on profile switch:** restore last mode if `remember_mode`, else reset to 0.
- **Capture mode:** while capturing, stop emitting output and send each physical
  key-down as a `KeyCaptured` signal (the GUI can't read the grabbed device itself).
- **LED:** OpenRazer D-Bus (`org.razer`) only; `openrazer-daemon` is required and the
  daemon refuses to start without it (decided 2026-11-19). No sysfs writes at runtime.
- **Permissions:** run as `systemd --user` service; udev rule tags Tartarus nodes
  and `/dev/uinput` with `uaccess`. No root.
- **Shutdown:** release all held keys, ungrab, destroy uinput device.

## Mode key: tap = next mode, hold = Hypershift

One physical key (`KEY_LEFTALT`, the hyper/mode key) does both jobs, decided by
**tap vs hold** (QMK "hold on other key press" semantics):

```
            mode key ↓
  Idle ───────────────────► Pending(t0)
   ▲                          │
   │   other input ↓          │  mode key ↑
   │   (key, wheel, button)   │
   │          ▼               ▼
   │      Shifted        dt = now − t0
   │   (lookup in        dt ≤ tap_term → NextMode (+ LED)
   │    hypershift        dt >  tap_term → cancel (no-op)
   │    layer)                │
   │          │ mode key ↑    │
   └──────────┴───────────────┘
```

- The mode key never emits anything itself, so the decision is lazy and costs
  **zero latency**: the first other input while held commits to Hypershift
  immediately; tap is decided on release.
- `tap_term` (default 350 ms; 200 ms proved too tight in practice) only separates a deliberate tap from a long
  press-and-release with no other key (cancel).
- Hypershift **never changes `mode`** and never touches the mode LED state; it
  reads `modes[mode].hypershift`.
- Unmapped keys in the Hypershift layer fall through to `modes[mode].bindings`
  (configurable: `hypershift_fallthrough = true`).
- Optional `hypershift_led_mask`: LED shown while shifted, restored to the mode's
  mask on release.
- Hypershift bindings may use `SetMode(n)` for direct mode selection
  (e.g. Hyper+1 → mode 0).
- Held-key invariant still governs releases: a key pressed while Shifted releases
  its recorded action even if the mode key is released first, and vice versa.

Global config:

```toml
mode_key = "KEY_LEFTALT"
tap_term_ms = 350
hypershift_fallthrough = true
# hypershift_led_mask = 7
```

## Per-key lighting (per mode, per Hypershift layer)

Each mode carries a static RGB frame; each mode's Hypershift layer may carry its
own frame. The daemon swaps frames on mode change and on Hypershift enter/exit.

```
Mode { led_mask, bindings, hypershift,
       lighting: Frame, hypershift_lighting: Option<Frame> }
Frame { default: Rgb, keys: Map<PhysicalKey, Rgb> }   // rendered to the matrix
LedMap: PhysicalKey → (row, col)                       // hardware constant, from discovery
```

TOML:

```toml
[mode.0.lighting]
default = "#000000"
KEY_W = "#00ffff"; KEY_A = "#00ffff"; KEY_S = "#00ffff"; KEY_D = "#00ffff"

[mode.0.hypershift_lighting]
default = "#101010"
KEY_1 = "#ff0000"
```

- Output path: OpenRazer kernel driver sysfs — `matrix_custom_frame` (per row:
  `row, start_col, end_col, RGB…`, one `write(2)` per row) then `matrix_effect_custom`.
  The OpenRazer Python client is not installed on the target; if `openrazer-daemon`
  is running, detect it and use its D-Bus custom-frame API instead so it doesn't
  overwrite state.
- Cell map discovery tool: `tools/led_probe.py`.
- Lighting writes run on a separate task fed by a latest-wins channel (`tokio::sync::watch`):
  the input path never waits on USB, and rapid Hypershift taps coalesce into the final frame.
- Always write the **full frame** (all 4 rows × 6 cols concatenated, one `write(2)`,
  84 bytes) then apply. Partial frames are unreliable; see `docs/hardware.md`.
- Static frames only in v1; firmware effects (wave/reactive/etc.) per mode can come later
  as an enum delegating to OpenRazer.
- Matrix dimensions and the `PhysicalKey → (row, col)` map are verified in Milestone 0
  (light one cell at a time, record which key lights).

## D-Bus interface (`org.tartarusd.Daemon1`, session bus, `zbus`)

```
Methods:  ListProfiles() → as
          ActivateProfile(s name)       # pins
          Unpin()
          GetState() → (s profile, y mode, b pinned)
          SetMode(y)
          StartCapture() / StopCapture()
Signals:  ProfileChanged(s)  ModeChanged(y)  ShiftChanged(b)  KeyCaptured(s physical_key)
```

Implemented in milestone 4. Object path `/org/tartarusd/Daemon1`; the daemon claims the
well-known name `org.tartarusd.Daemon1` before grabbing the device, so a second instance
fails fast. The typed client proxy lives in `crates/ipc`.

- `ShiftChanged(true/false)` fires when Hypershift is entered/left (mode key held with
  another input / released), so a bar widget can show it.
- `ActivateProfile` and `SetMode` reject unknown names / out-of-range modes with
  `InvalidArgs`. `Unpin` returns to the profile chosen at startup.
- **Capture** suspends remapping (held output keys are released first) and reports
  every physical key-down, including the mode key, as `KeyCaptured`. It ends on
  `StopCapture` or on its own 30 s after the last `StartCapture`, so a crashed client
  can't leave the device dead.
- Signals are emitted from a separate task; the input path never waits on the bus.

## CLI (`crates/cli`) — `tartarusctl`

`state`, `list`, `activate <name>`, `unpin`, `mode <n>`, `validate <file>` (offline, no
daemon needed), and `monitor` (prints signals as they happen, for debugging and scripts).

## GUI (`crates/gui`)

- Toolkit: `gtk4-rs` + libadwaita via `relm4` (default choice; `slint`/`iced` acceptable).
- Visual Tartarus layout; mode tabs (blue/green/red); profile list sidebar.
- Click key → choose binding: record chord (captured from the regular keyboard via
  GTK, fine because only the Tartarus is grabbed), macro editor, or mode action.
- Physical-key identification through daemon `StartCapture()` + `KeyCaptured`.
- Profile editor for match rules with a "use current window" button (available only when a window-source backend,
  e.g. `hyprland`, is compiled in).
- Import from input-remapper presets → `mode.0` of a new profile.

## Workspace layout

```
Cargo.toml            # workspace
crates/
  core/    # model, serde, validation, input-remapper importer — NO I/O
  daemon/  # evdev, uinput, zbus server; window sources behind cargo features (hyprland)
  cli/     # tartarusctl
  gui/     # relm4 app, zbus client
packaging/
  70-tartarusd.rules      # udev (uaccess)
  tartarusd.service       # systemd --user unit
docs/
  hardware.md             # verified Milestone 0 findings
```

Key crates: `evdev` (read + uinput `VirtualDevice`), `zbus`, `serde` + `toml`,
`notify`, `arc-swap`, `tokio`, `thiserror`/`anyhow`, `tracing`, `relm4`.

## Milestones

0. **Hardware discovery** — mostly done (see `docs/hardware.md`). Remaining: run
   `tools/led_probe.py` for the per-key matrix map; inspect an input-remapper preset.
1. **core** — model, TOML (de)serialization, validation, lookup logic; unit tests for
   the state machine incl. the held-key release invariant.
2. **daemon MVP** — grab, 3-mode cycle, passthrough + chord actions, LED, single
   hard-coded profile path.
3. **profiles + reload** — profiles dir, inotify reload, atomic swap.
4. **D-Bus + CLI.**
5. **Window sources + opt-in `hyprland` feature** (auto-switch). Done.
6. **Macros + hypershift layer.**
6b. **Per-key lighting** — frames per mode and per Hypershift layer.
7. **GUI** (incl. per-key color editing on the same layout widget).
8. **input-remapper importer.**
9. **Packaging** — udev rule, systemd user unit, README.

## Invariants / tests

- Every emitted key-down has exactly one matching key-up, across mode switches,
  profile swaps, capture start, and shutdown.
- Mode key: tap (no other input, dt ≤ tap_term) → mode+1; any other input while held
  → Hypershift, mode unchanged; long hold with no other input → no-op.
- Invalid profile file → keep previous tables, log error, emit no partial state.
- `core` is pure; test it with simulated event sequences (property tests welcome:
  random interleavings of key/mode/profile events must leave no held keys after
  all physical keys are released).
