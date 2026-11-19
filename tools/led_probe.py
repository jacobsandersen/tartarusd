#!/usr/bin/env python3
"""Map Tartarus V2 per-key LED matrix cells to physical keys via OpenRazer sysfs.

Matrix is 4 rows x 6 cols. Partial-row writes do not display reliably, so every
step writes the FULL frame (all rows, one write(2)) and then applies it.

    sudo python3 -I tools/led_probe.py

Input per cell:  key label (e.g. 01, 20, wheel, dpad)  |  Enter = nothing lit
                 q = quit
Prints a TOML [led_map] table at the end.
"""
import glob
import os
import sys

ROWS, COLS = 4, 6
WHITE, OFF = (255, 255, 255), (0, 0, 0)

paths = glob.glob("/sys/bus/hid/drivers/razerkbd/0003:1532:022B.*/matrix_custom_frame")
if not paths:
    sys.exit("matrix_custom_frame not found for 1532:022B")
d = os.path.dirname(paths[0])


def write(attr: str, data: bytes) -> bool:
    fd = os.open(os.path.join(d, attr), os.O_WRONLY)
    try:
        return os.write(fd, data) == len(data)  # sysfs store needs a single write
    except OSError as e:
        print(f"  {attr}: write failed: errno {e.errno} {e.strerror}", file=sys.stderr)
        return False
    finally:
        os.close(fd)


def show(lit: tuple[int, int] | None) -> bool:
    frame = bytearray()
    for r in range(ROWS):
        frame += bytes([r, 0, COLS - 1])
        for c in range(COLS):
            frame += bytes(WHITE if (r, c) == lit else OFF)
    return write("matrix_custom_frame", bytes(frame)) and write("matrix_effect_custom", b"1")


found: list[tuple[int, int, str]] = []
try:
    for r in range(ROWS):
        for c in range(COLS):
            if not show((r, c)):
                continue
            ans = input(f"({r},{c}) lit which key? ").strip()
            if ans == "q":
                raise KeyboardInterrupt
            if ans:
                found.append((r, c, ans))
except KeyboardInterrupt:
    pass
finally:
    show(None)

print("\n[led_map]  # label = [row, col]")
for r, c, label in found:
    print(f'"{label}" = [{r}, {c}]')
