# tartarusd

A userspace remapping daemon for the **Razer Tartarus V2** on Linux. It restores the
Synapse-style keymaps that the Tartarus has on Windows:

- **Modes.** Up to 7 keymaps per profile, shown on the three profile LEDs (blue, green,
  red, then combinations).
- **Hypershift.** The hyper/mode key shifts into a per-mode second layer without leaving
  the mode.
- **Per-key lighting.** Each mode, and each mode's Hypershift layer, can have its own
  static key colors.
- **Per-game profiles.** Plain TOML files, reloaded live when they change, optionally
  switched automatically by the focused window.
- **Macros, chords, wheel and D-pad remapping.**

It replaces input-remapper for this device. It runs as a `systemd --user` service with no
root and no kernel code, on any desktop environment. Window-based profile switching is an
opt-in build feature (Hyprland is the first supported compositor).

> **Status:** the daemon, CLI, profiles, lighting, macros, the input-remapper importer and
> the GTK4 profile editor are working. The editor is new and has had little hands-on testing;
> profiles can always be edited by hand.

## Requirements

- Linux with a Razer Tartarus V2 (`1532:022b`).
- [OpenRazer](https://openrazer.github.io/): the kernel driver **and** `openrazer-daemon`
  running. tartarusd drives the profile LEDs and key lighting through its D-Bus API and
  refuses to start without it.
- Access to `/dev/uinput` and the Tartarus input nodes for your user (the udev rule below).
- Rust nightly to build (pinned in `rust-toolchain.toml`; `rustup` fetches it).
- `systemd --user` if you want to run it as a service (optional).
- GTK4 and libadwaita development packages, only if you build the editor (`--gui`).

## Install

### Arch Linux (pacman)

```sh
cd packaging/arch && makepkg -si
```

This builds `tartarusd-git` (daemon, CLI, user unit, udev rule) and `tartarusd-gui-git` (the
editor) from this checkout's committed HEAD, so pacman tracks the files and can remove them.
It needs the `rust` package to build (pacman installs it as a build dependency; remove it
afterwards if you like) and network access while building. Hyprland window switching is built in; run
`TARTARUSD_FEATURES= makepkg -si` to leave it out. After installing, follow the notes pacman
prints (re-plug the device, add a profile, `systemctl --user enable --now tartarusd`). The
udev rule is installed by the package, so the `sudo install` step below isn't needed.

### Any distribution

```sh
packaging/install.sh                       # build and install to ~/.local
packaging/install.sh --features hyprland   # ...with Hyprland window-based switching
packaging/install.sh --gui                 # ...and the GTK4 profile editor
```

This installs `tartarusd` and `tartarusctl` to `~/.local/bin`, the user unit to
`~/.config/systemd/user/`, and creates `~/.config/tartarusd/` with a commented
`config.toml` (an existing one is left alone). It never uses root and never starts
anything. Then:

1. **Device access (once, needs root):**
   ```sh
   sudo install -Dm644 packaging/70-tartarusd.rules /etc/udev/rules.d/70-tartarusd.rules
   sudo udevadm control --reload
   sudo udevadm trigger --subsystem-match=input --subsystem-match=misc
   ```
2. **Add a profile** to `~/.config/tartarusd/profiles/` (see below).
3. **Stop input-remapper from grabbing the Tartarus.** Only one program can hold the
   device. Disable its autoload/injection for "Razer Razer Tartarus V2".
4. **Start it:**
   ```sh
   systemctl --user daemon-reload
   systemctl --user enable --now tartarusd.service
   journalctl --user -u tartarusd -f      # logs
   ```

To remove a manual install: `packaging/uninstall.sh` (keeps `~/.config/tartarusd`).

### Try it first, safely

Before enabling the service, run it in the foreground with a built-in time limit. It
releases the device when it exits, and Ctrl-C does the same:

```sh
cargo run --release -p tartarusd -- --profiles-dir docs/examples --profile "Smoke test" --exit-after 60
```

[`docs/examples/smoke-test.toml`](docs/examples/smoke-test.toml) maps key 01 to a letter per
mode, key 02 to a typed macro, and lights a few keys, so each feature is visible at a glance.

## Profiles

One TOML file per profile in `~/.config/tartarusd/profiles/`. Files are the single source of
truth: edit one and the daemon reloads it within a fraction of a second. An invalid edit is
logged and ignored, so the last good version keeps running.

```toml
name = "Elden Ring"
remember_mode = true          # restore the last mode when switching back to this profile

[[match]]                     # auto-switch when this window is focused (needs a window backend)
class = "steam_app_1245620"

[mode.0]
led_mask = 1                  # optional; defaults to blue, green, red, then combinations
KEY_1 = "KEY_R"               # a key becomes a key
KEY_2 = "KEY_LEFTCTRL+KEY_C"  # a chord
KEY_3 = "disabled"            # swallow it
KEY_4 = "next_mode"
KEY_5 = { set_mode = 2 }
KEY_Q = { macro = [["KEY_E", 0], ["KEY_F", 50]] }   # tap E, then F 50 ms later
WHEEL_UP = "KEY_PAGEUP"

[mode.0.hypershift]           # the layer while the mode key is held
KEY_1 = "KEY_F1"

[mode.0.lighting]             # static key colors for this mode
default = "#000000"
KEY_W = "#00ffff"

[mode.0.hypershift_lighting]  # ...and while shifted
default = "#101010"
KEY_1 = "#ff0000"

[mode.1]
[mode.1.effect]               # or a firmware effect instead of key colors (not both)
type = "wave"
direction = "left"
[mode.2]
```

- **Keys** are Linux names (`KEY_A`, `KEY_LEFTSHIFT`, `BTN_MIDDLE`, `KEY_UP`, ...) plus
  `WHEEL_UP` / `WHEEL_DOWN`. Unbound keys pass through unchanged.
- **Modes** are `[mode.0]`, `[mode.1]`, ...: up to 3 by default, up to 7 with
  `extended_modes = true`.
- **Macros** must be balanced (every key they press is released by the end). Each step is
  `[key, delay_ms]` (a tap) or `[key, "down"|"up", delay_ms]`.
- **Firmware effects** (`spectrum`, `wave`, `breathing`, `reactive`, `starlight`) can replace
  key colors on a mode or its Hypershift layer; see [DESIGN.md](DESIGN.md#firmware-effects)
  for options. The pad runs them itself, so they use no CPU and can't show per-key colors.
- **Lighting is per profile, opt-in.** A profile with no `lighting` never touches the key
  matrix. One that defines any owns it: modes without a frame go dark. Switching to a
  profile without lighting hands the pad back to OpenRazer's own effect.
- Check a file without the daemon: `tartarusctl validate my-profile.toml`.

The full format, the rules for each field, and the design behind them are in
[DESIGN.md](DESIGN.md). Verified hardware facts (key codes, LED matrix) are in
[docs/hardware.md](docs/hardware.md).

## The editor

`tartarusd-gui` (installed with `--gui`, launcher "Tartarus Profiles") edits the same profile
files without hand-editing: pick a profile, pick a mode, click a key on the drawn pad and choose
what it does (a key or chord you can record from the keyboard or type with completion: "shift" then Tab gives `KEY_LEFTSHIFT`; a macro; switch mode; disable).
The **Hypershift** toggle edits the layer used while the mode key is held, and the **Lighting**
view paints per-key colors for each mode and layer: click a key, Ctrl-click to add more, or drag a
box across the pad (Ctrl+A selects all), then pick a color. Colors apply as you drag the sliders,
and Copy/Paste color (also Ctrl+C / Ctrl+V) moves a color between keys. With the daemon running,
the pad shows the lighting you are editing live, before you save. Mode tabs, profile LEDs, match rules,
new/delete and **Import input-remapper preset** are all there. **Save** (Ctrl+S) writes the
file atomically and the daemon reloads it.

It works without the daemon. When the daemon is running it also shows the active profile and
mode, can pin a profile (menu), and **Identify key on pad** selects the key you press on the
Tartarus. Not there yet: a "use the current window" button for match rules, and per-key colors
for the D-pad and wheel directions (they have no LED).

## Migrating from input-remapper

```sh
tartarusctl import ~/.config/input-remapper-2/presets/"Razer Razer Tartarus V2"/mine.json \
    --name "Mine" --modes 3 --out ~/.config/tartarusd/profiles/mine.toml
```

The preset's single-key mappings become the base layer of mode 0, and its `Hyper+<key>`
combinations become the mode-0 Hypershift layer. Anything it can't convert (wheel mappings,
input-remapper macro syntax, key combinations, unknown symbols) is listed as a warning on
stderr and skipped, never guessed, so fix those by hand in the result. Letters and keysym
names are read as a US layout. Without `--out` the profile goes to stdout. With `--modes 2`
or `3`, the extra modes start empty (keys pass through). Check the result with
`tartarusctl validate`. The editor's import does the same conversion.

## The mode key

By default (`mode_switch = "double_tap"`), pressing the mode key shows Hypershift
immediately, and **two quick taps** in a row cycle the mode. With `mode_switch = "tap"`, a
single quick tap cycles instead, and Hypershift starts when another key is pressed or the
key has been held for a moment. Neither adds any delay to other keys. Set it in
`config.toml`:

```toml
mode_switch = "double_tap"   # or "tap"
tap_term_ms = 350            # what counts as quick
```

All options, with their defaults, are in
[`packaging/config.example.toml`](packaging/config.example.toml). The config is read at
startup, so restart the service after editing it.

## Automatic profile switching

Build with `--features hyprland` and add `[[match]]` rules to your profiles (the editor shows
the rules section only in builds with the feature). A rule matches
the focused window by `class` (exact) and/or `title_regex`; every field set must match, and
the first matching profile (by file name) wins. Selection is:

> pinned profile, then first matching profile, then your default profile

Without a window backend, or on another desktop, everything else works: profiles are chosen
by `default_profile` or pinned with `tartarusctl`, and `[[match]]` rules are kept but unused,
so the same profiles stay portable.

## Controlling it

```sh
tartarusctl state             # active profile, mode, pinned?
tartarusctl list              # loaded profiles
tartarusctl activate "Name"   # pin a profile
tartarusctl unpin             # back to automatic / default
tartarusctl mode 2            # jump to a mode (0-based)
tartarusctl validate FILE     # check a profile file (works without the daemon)
tartarusctl monitor           # print mode/profile/Hypershift changes as they happen
```

The daemon exposes the same on the session bus as `org.tartarusd.Daemon1`, including a
`ShiftChanged(bool)` signal that a status-bar widget can use to show when Hypershift is
active.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `grabbing ...: Device or resource busy` | input-remapper (or another grabber) holds the Tartarus. Stop it for this device. |
| `openrazer-daemon is required ...` | Start `openrazer-daemon` (a user service on most distros) and load the OpenRazer driver. |
| `creating uinput devices` fails | Your user can't open `/dev/uinput`. Install the udev rule and re-trigger, or log in again. |
| `claiming org.tartarusd.Daemon1 ... another tartarusd running?` | A second instance is running; stop it first. |
| `tartarusctl: tartarusd is not running` | The daemon isn't on the session bus. Check `systemctl --user status tartarusd`. |
| Profiles don't switch with the window | The daemon was built without `--features hyprland`, or isn't seeing Hyprland (check the startup log line `window source active`). |

If the daemon ever misbehaves, `systemctl --user stop tartarusd` releases the device
immediately, and the Tartarus goes back to behaving as a plain keyboard.

## Development

```sh
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features tartarusd/hyprland,tartarusd-gui/hyprland -- -D warnings
cargo test
cargo test -p tartarusd --features hyprland live -- --ignored   # needs Hyprland (read-only)
cargo test -p tartarusd live_frame -- --ignored                 # briefly lights the pad
```

The workspace is `crates/core` (pure model and state machine, no I/O), `crates/daemon`,
`crates/ipc` (D-Bus names and client proxy) and `crates/cli`. Start with
[CLAUDE.md](CLAUDE.md) for the ground rules and [DESIGN.md](DESIGN.md) for the design.

## Releases

[release-please](https://github.com/googleapis/release-please) runs on every push to `main`.
It reads [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`,
`feat!:` ...), keeps a release pull request open with the next version and `CHANGELOG.md`,
and tags the release when that pull request is merged. The version lives in the root
`Cargo.toml`; the workflow also syncs `Cargo.lock` on the pull request. In the repository
settings, allow GitHub Actions to create pull requests.

## License

Not chosen yet.
