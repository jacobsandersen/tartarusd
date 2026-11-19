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

## Macros

A macro is a list of steps (`key`, `down`/`up`, `delay_ms` before the step), run by the
state machine itself: `Machine::tick(now)` performs steps that are due, and
`next_deadline()` tells the daemon when to wake. There is no separate task or timer
thread, so macros are deterministic under test and cancellation is trivial.

- A macro starts on key-**down**; releasing the physical key does nothing. A zero-delay
  first step runs inside the press itself (no added latency).
- Steps use the same reference-counted output as physical keys, so overlapping macros
  and held keys can never produce a double press or an orphaned release.
- Macros must be **balanced** (validated at load): every key pressed is released by the
  end, none is pressed twice, and none is released without being pressed. Max 512 steps.
- Several macros can run at once (cap 32; further presses are ignored).
- Profile swap, capture start and shutdown **cancel** running macros and release their
  keys. Changing mode does not.
- If a tick is late, due steps run in order and the next delay counts from the actual
  tick time, so a stall never produces a burst of back-to-back steps.
- Not supported yet: repeat-while-held, and macros that run another action.

## Mode key: Hypershift and mode switching

One physical key (`KEY_LEFTALT`, the hyper/mode key) both shifts into the Hypershift layer
and switches modes. Which gesture switches modes is a global option, `mode_switch`:

| `mode_switch` | Press | Switch modes |
|---|---|---|
| `double_tap` (default) | shows Hypershift immediately | two quick taps in a row |
| `tap` | undecided until another input or `tap_term` | one quick tap |
| `select` | shows Hypershift immediately | two quick taps open a timed selection (below) |

Either way the mode key never emits anything itself, and neither gesture adds latency
to other keys. "Quick" means shorter than `tap_term` (default 350 ms; 200 ms proved too
tight in practice).

### `double_tap` (default): Hypershift first

```
                       press
  Idle ───────────────────────────► Shifted   (Hypershift layer active at once;
   ▲                                   │        LED, lighting and ShiftChanged follow)
   │                                   │ release
   │      quick press (< tap_term)     ▼
   │      with nothing else pressed    ├─ first tap ──► Idle, armed for tap_term
   │                                   ├─ second tap, within tap_term of the first
   │                                   │     ──► Idle, NextMode (+ LED, lighting)
   └───────────────────────────────────┴─ anything else (held long, another input
                                           during the hold) ──► Idle, nothing armed
```

- Pressing the key shows Hypershift immediately, so a hold needs no timer. A single tap
  therefore flashes Hypershift for as long as the key is down, and changes nothing else.
- A double tap needs **two consecutive taps**: each shorter than `tap_term`, with the gap
  between them (release to next press) also shorter than `tap_term`, and with no other
  input in between or during either tap. Any other input cancels it.
- Pressing another key while the mode key is down is an ordinary Hypershift use and
  arms nothing.

### `select`: choose a mode

Opt-in. Pressing and tapping work as in `double_tap`, but the second quick tap opens a
**selection** instead of cycling:

- The profile LED blinks the candidate mode (starting on the current one). D-pad up and
  down move the candidate (wrapping); the LED shows it. The real mode, bindings and
  lighting do not change yet.
- One quick tap of the mode key picks the candidate. A hold does nothing.
- Every other input is swallowed. With no navigation for `select_timeout_ms` (default
  4000) the selection is cancelled and the real mode's LED returns.
- Profile swap and D-Bus `SetMode` also close it. A profile with one mode never opens it.
- The blink is driven by `Machine::tick`/`next_deadline`, like macros (300 ms per phase).

### `tap`: mode switch first

```
            mode key ↓
  Idle ───────────────────► Pending(t0)
   ▲                          │
   │   other input ↓          │  mode key ↑            held until t0 + tap_term
   │   (key, wheel, button)   │                        with nothing else pressed
   │          ▼               ▼                                │
   │      Shifted        dt = now − t0                         │
   │   (lookup in        dt <  tap_term → NextMode (+ LED)     │
   │    hypershift        dt >= tap_term → cancel (no-op)      │
   │    layer)                │                                │
   │          ▲───────────────┼────────────────────────────────┘
   │          │ mode key ↑    │
   └──────────┴───────────────┘
```

- The first other input while held commits to Hypershift immediately; a tap is decided on
  release.
- A hold also commits **by time**: once the key has been held for `tap_term` with
  nothing else pressed, a timer promotes it to Hypershift, so the LED, lighting and
  `ShiftChanged` signal show it without needing another key. Releasing a promoted hold
  never changes the mode. (A short tap can't flicker into Hypershift, because promotion
  only happens after `tap_term`.)
- `tap_term` is the boundary: released **before** it is a tap, at or after it is a hold.

### Both gestures

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
mode_switch = "double_tap"   # or "tap" or "select"
tap_term_ms = 350
# select_timeout_ms = 4000  # "select" only
hypershift_fallthrough = true
# hypershift_led_mask = 7
```

## Per-key lighting (per mode, per Hypershift layer)

A layer's lighting is either per-key colors (this section) or one **firmware effect**
(next section), never both.

Each mode may carry a static RGB frame, and its Hypershift layer may carry its own. The
state machine emits the frame to show whenever it can change: mode change, Hypershift
enter/exit, profile swap.

```
Mode  { ..., lighting: Option<Frame>, hypershift_lighting: Option<Frame> }
Frame { default: Rgb, keys: Map<Key, Rgb> }   // keys that have an LED
```

```toml
[mode.0.lighting]
default = "#000000"
KEY_W = "#00ffff"; KEY_A = "#00ffff"; KEY_S = "#00ffff"; KEY_D = "#00ffff"

[mode.0.hypershift_lighting]
default = "#101010"
KEY_1 = "#ff0000"
```

- **Which keys:** the 21 keys with LEDs (01-20 and the wheel). The wheel's LED is
  `BTN_MIDDLE`; the D-pad and the mode key have none and are rejected at load, as are
  unknown keys and anything that is not `#rrggbb`. The cell map is a hardware constant
  in `core` (`lighting::cell`), taken from `docs/hardware.md`.
- **Opt-in per profile:** a profile with no `lighting` anywhere never touches the matrix.
  Once a profile defines any frame, it owns the pad: a mode with no frame of its own
  shows a dark one. Hypershift without a `hypershift_lighting` keeps the mode's frame.
- **Handing the pad back:** switching to a profile with no lighting, and daemon shutdown,
  call OpenRazer's `restoreLastEffect` (documented for exactly this: applications that
  use custom frames, after they exit) so the pad returns to the effect you configured.
- **Output path:** OpenRazer D-Bus only. `setCustom` once, then each frame is one
  `setKeyRow` call carrying the full 84 bytes (4 rows x `row, 0, 5, 6 x RGB`); no reset
  or blanking between frames (verified, `docs/hardware.md`). Identical consecutive frames
  are not rewritten.
- **Never blocks input:** frames go to a separate task over a latest-wins `watch`
  channel, so rapid Hypershift taps coalesce into the final frame. A failed write forgets
  the believed pad state, so the next frame re-enters custom mode and rewrites in full.
- Static frames only in v1; firmware effects (wave/reactive/etc.) per mode can come later
  as an enum delegating to OpenRazer.

## Firmware effects

Instead of key colors, a mode or its Hypershift layer can run one of the pad's built-in
effects. They cover the whole pad and are run by the firmware, so they cost nothing while
running; they cannot be mixed with per-key colors or react to macros (a macro-driven
animation was considered and dropped).

```toml
[mode.1.effect]                 # or [mode.1.hypershift_effect]
type = "starlight"              # spectrum | wave | breathing | reactive | starlight
colors = ["#ff00ff", "#00ffff"] # breathing, starlight: 0 (random), 1 or 2 colors
speed = 2                       # reactive 1-4, starlight 1-3; 1 is fastest (default 2)
```

- `wave` takes `direction = "left" | "right"` (the way it travels; default left).
  `reactive` takes `color` and `speed`. `spectrum` takes nothing.
- A layer sets `lighting` or `effect`, not both; a profile that sets both for the same layer
  is rejected. Base and Hypershift may differ (colors on one, an effect on the other).
- Ownership rules are the same as for frames: a profile with any lighting or effect owns the
  pad, a mode with neither is dark, and Hypershift without its own keeps the mode's.
- **Hand-back:** OpenRazer remembers the last effect it was given, and effect calls
  overwrite it. Before its first effect call the daemon reads the configured effect and
  sets it again when handing the pad back (profile without lighting, shutdown). Switching
  to custom frames does not change the remembered effect. If the daemon is killed while an
  effect is showing, OpenRazer keeps that effect as its own.
- `docs/hardware.md` has the verified calls and argument order.

## D-Bus interface (`org.tartarusd.Daemon1`, session bus, `zbus`)

```
Methods:  ListProfiles() → as
          ActivateProfile(s name)       # pins
          Unpin()
          GetState() → (s profile, y mode, b pinned)
          SetMode(y)
          StartCapture() / StopCapture()
          PreviewFrame(ay) / PreviewEffect(s) / ClearPreview()
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
- **Preview** shows a client-rendered 84-byte frame on the pad (the same payload as
  `setKeyRow`), or a firmware effect given as TOML (`PreviewEffect`) so an editor can show lighting changes live without saving. While a preview
  is active the profile's own frames wait; `ClearPreview`, or 20 s without a renewing
  `PreviewFrame`, brings back the profile's current frame (or hands the pad back if the
  profile has no lighting). A frame of the wrong size is rejected with `InvalidArgs`.
- Signals are emitted from a separate task; the input path never waits on the bus.

## input-remapper importer (`core::import`)

Converts an input-remapper 2.x preset (a JSON array of mappings) into a tartarusd profile.
Pure; the CLI does the file I/O. Only forms confirmed in real presets are converted
(`docs/hardware.md`); everything else is **reported and skipped, never guessed**.

- A single-key mapping becomes a base-layer binding. `[mode key, key]` (the Tartarus
  Hypershift, `KEY_LEFTALT` by default) becomes the mode-0 Hypershift layer.
- `output_symbol`: `disable`, a keysym, a `KEY_*`/`BTN_*` name, or a `+` chord of those.
  Keysyms assume a US layout. An uppercase keysym is rejected with a hint to write
  `Shift_L+x`.
- Skipped with a message: other mapping types, wheel/relative inputs, input-remapper macro
  syntax, other key combinations, unknown key codes, unknown symbols, and duplicate bindings
  (the first wins).
- The preset's bindings go to mode 0; `--modes 2|3` adds empty modes (keys pass through).
- Fails only if the JSON is not a preset or nothing converts at all. The result is
  produced by `Profile::to_toml` and re-validated before it is written.

## CLI (`crates/cli`) — `tartarusctl`

`state`, `list`, `activate <name>`, `unpin`, `mode <n>`, `validate <file>` (offline, no
daemon needed), `import <preset.json>` (offline; see the importer section), and `monitor` (prints signals as they happen, for debugging and scripts).

## GUI (`crates/gui`)

- Toolkit: plain `gtk4-rs` + libadwaita (decided at implementation; `relm4` was the earlier
  default, but the editor is one window rebuilt from a single state struct, so a framework
  added little). No async runtime: D-Bus calls (zbus) are awaited on the GLib main loop.
- Structure: pure, tested modules (`edit` operations on `Profile`, `store` for the profiles
  directory, `text` for chord/macro text, `layout` for the drawn pad) under a thin `ui`
  layer. The profile file is written with `Profile::to_toml` after a load round-trip check.
- Works without the daemon (files only). Daemon features (status, pin, key capture) are
  polled every 2 s and disabled when it is absent.
- Visual Tartarus layout; mode tabs (blue/green/red); profile list sidebar.
- Click key → choose binding: record chord (captured from the regular keyboard via
  GTK, fine because only the Tartarus is grabbed), macro editor, or mode action.
- Physical-key identification through daemon `StartCapture()` + `KeyCaptured`.
- Profile editor for match rules with a "use current window" button (available only when a window-source backend,
  e.g. `hyprland`, is compiled in).
- Import from input-remapper presets → `mode.0` of a new profile. (The conversion itself
  is `tartarusctl import` / `core::import`; the GUI calls it.)

## Workspace layout

```
Cargo.toml            # workspace
crates/
  core/    # model, serde, validation, input-remapper importer — NO I/O
  daemon/  # evdev, uinput, zbus server; window sources behind cargo features (hyprland)
  cli/     # tartarusctl
  gui/     # gtk4 + libadwaita editor, zbus client
packaging/
  70-tartarusd.rules      # udev (uaccess)
  tartarusd.service       # systemd --user unit
docs/
  hardware.md             # verified Milestone 0 findings
```

Key crates: `evdev` (read + uinput `VirtualDevice`), `zbus`, `serde` + `toml`,
`notify`, `arc-swap`, `tokio`, `thiserror`/`anyhow`, `tracing`, `relm4`.

## Milestones

0. **Hardware discovery** — done (see `docs/hardware.md`): key codes, LED matrix map,
   live frame updates, D-Bus path, input-remapper preset schema.
1. **core** — model, TOML (de)serialization, validation, lookup logic; unit tests for
   the state machine incl. the held-key release invariant.
2. **daemon MVP** — grab, 3-mode cycle, passthrough + chord actions, LED, single
   hard-coded profile path.
3. **profiles + reload** — profiles dir, inotify reload, atomic swap.
4. **D-Bus + CLI.**
5. **Window sources + opt-in `hyprland` feature** (auto-switch). Done.
6. **Macros + hypershift layer.** Done (Hypershift landed with the core state machine).
6b. **Per-key lighting** — frames per mode and per Hypershift layer. Done.
7. **GUI** (incl. per-key color editing on the same layout widget). Done, except the
   "use current window" button for match rules.
8. **input-remapper importer.** Done: `core::import` + `tartarusctl import`.
9. **Packaging** — udev rule, systemd user unit, installer, README. Done.

## Invariants / tests

- Every emitted key-down has exactly one matching key-up, across mode switches,
  profile swaps, capture start, and shutdown.
- Mode key: tap (no other input, dt ≤ tap_term) → mode+1; any other input while held
  → Hypershift, mode unchanged; long hold with no other input → no-op.
- Invalid profile file → keep previous tables, log error, emit no partial state.
- `core` is pure; test it with simulated event sequences (property tests welcome:
  random interleavings of key/mode/profile events must leave no held keys after
  all physical keys are released).
