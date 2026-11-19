# Razer Tartarus V2 — verified hardware facts (Milestone 0)

Verified on the target machine, 2026-11-19.

## USB / HID

- VID:PID `1532:022B`, name `"Razer Razer Tartarus V2"` (identical on all nodes).
- 3 HID interfaces:

| USB if | HID instance | evdev (stable path under `/dev/input/by-id/`) | Emits |
|---|---|---|---|
| if00 | `0003:1532:022B.0003` | `usb-Razer_Razer_Tartarus_V2-event-kbd` | nothing observed |
| if01 | `0003:1532:022B.0004` | `usb-Razer_Razer_Tartarus_V2-if01-event-kbd` | all keys, mode key, D-pad |
| if02 | `0003:1532:022B.0005` | `usb-Razer_Razer_Tartarus_V2-if02-event-mouse` | scroll wheel + wheel click |

- HID instance numbers and `eventN` numbers are not stable; always resolve via
  `/dev/input/by-id/` (evdev) and a glob on `0003:1532:022B.*` (sysfs).
- evtest's capability list is the HID report descriptor's declared range, not the
  physical key set. Use the table below.
- Grab all three nodes (if00 included, in case it emits in some state).

## Key codes (default, no remapping)

| Physical | Code | Node |
|---|---|---|
| 01–05 | `KEY_1` … `KEY_5` | if01 |
| 06–10 | `KEY_TAB`, `KEY_Q`, `KEY_W`, `KEY_E`, `KEY_R` | if01 |
| 11–15 | `KEY_CAPSLOCK`, `KEY_A`, `KEY_S`, `KEY_D`, `KEY_F` | if01 |
| 16–19 | `KEY_LEFTSHIFT`, `KEY_Z`, `KEY_X`, `KEY_C` | if01 |
| 20 (large thumb button) | `KEY_SPACE` | if01 |
| Mode / hyper key (Hypershift on Windows) | `KEY_LEFTALT` | if01 |
| D-pad left/right/up/down | `KEY_LEFT`, `KEY_RIGHT`, `KEY_UP`, `KEY_DOWN` | if01 |
| Wheel up / down | `REL_WHEEL ±1` **and** `REL_WHEEL_HI_RES ±120` (same SYN frame) | if02 |
| Wheel click | `BTN_MIDDLE` | if02 |

Implications:
- Mode key is a real modifier code (`KEY_LEFTALT`); the daemon must consume it and
  never forward it.
- Remapping the wheel to keys must consume **both** `REL_WHEEL` and
  `REL_WHEEL_HI_RES` per frame; forward both unchanged on passthrough.
- `PhysicalKey` must cover EV_KEY codes from two nodes and EV_REL wheel directions
  (e.g. `WheelUp`, `WheelDown`).

## Profile LEDs

- sysfs attrs on the if02 HID instance: `profile_led_red`, `profile_led_green`,
  `profile_led_blue`.
- Independent bits; any combination can be lit → 8 states (mask R=4, G=2, B=1).
- Power-on state: blue only.

## Per-key matrix

- 4 rows × 6 cols, sysfs on the if02 HID instance (verified 2026-11-19).
- Row → physical keys (whole-row tests):
  - row 0: 01–05 · row 1: 06–10 · row 2: 11–15
  - row 3: 16–19, 20 (thumb), scroll wheel
  - rows 4–5: nothing
- Cell map (verified with `tools/led_probe.py`):

  | row \ col | 0 | 1 | 2 | 3 | 4 | 5 |
  |---|---|---|---|---|---|---|
  | 0 | 01 | 02 | 03 | 04 | 05 | — |
  | 1 | 06 | 07 | 08 | 09 | 10 | — |
  | 2 | 11 | 12 | 13 | 14 | 15 | — |
  | 3 | 16 | 17 | 18 | 19 | scroll wheel | 20 (thumb) |

  21 LEDs; (0..2, 5) are unused. The D-pad has no per-key LED.
- Rows wider than 6 columns: the first row still displays but later rows in the same
  write do not. Always send exactly 6 columns.
- `matrix_custom_frame` per row: `row, start_col, end_col, RGB × n`; send all 4 rows
  concatenated in ONE write, then write `1` to `matrix_effect_custom`.
- Verified working sequence: `matrix_effect_none` → full frame → `matrix_effect_custom`.
  Row order in the write does not matter; a 100 ms delay or a repeated write changes nothing.
- Partial frames (single rows) gave unreliable results; always send the full frame.
- **Live update VERIFIED (H2/H3, `tools/led_live_test.py`):** `matrix_effect_custom` is
  needed only ONCE to put the pad into custom mode. After that, writing a new full
  frame to `matrix_custom_frame` alone shows immediately: no `effect_none`, no
  re-apply. The daemon must never blank between frames.
- Driver (openrazer-driver 3.12.4) sends one USB report per row, transaction id 0x1F
  (extended matrix); quirks are device firmware behavior.
- `matrix_effect_static` (3 bytes RGB) and `matrix_effect_none` work.
- `matrix_brightness` reads/writes ASCII `0`–`255`.

## Profile LED semantics

- Three physically separate LEDs (blue, green, red), not a color-mixing RGB LED. Mixed
  states mean several LEDs lit at once. Modes 0–2 map to B, G, R; modes 3+ are
  combinations of lit LEDs.

## input-remapper preset schema (from `docs/inputremapper-tartarus-sample.json`)

- File is a JSON array of mappings. Each: `input_combination` (list of
  `{type, code, origin_hash}`; `type` 1 = EV_KEY, `code` = Linux key code),
  `target_uinput`, `output_symbol` (`"disable"` here), `name`, `mapping_type`.
- A multi-element combination is a chord. Hypershift appears as `[56, N]` where 56 =
  `KEY_LEFTALT` (named `Hyper+NN`). Importer: leading code 56 → Hypershift layer.
- `origin_hash` identifies the source device; ignore it on import.
- `name` is the physical key label (`01`–`20`, `zzz. Left/Right/Up/Down` for the D-pad).
- The sample has only `disable` mappings and no wheel mappings, so real
  `output_symbol` forms (key names, macros) are still unseen; ask for one with real
  bindings before writing the importer's symbol parsing.
