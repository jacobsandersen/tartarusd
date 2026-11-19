#!/usr/bin/env python3
"""Diagnose per-key lighting on the Tartarus V2 via OpenRazer sysfs.

    sudo python3 -I tools/led_diag.py

Each step prints OK or the errno and pauses so you can note what the pad shows.
"""
import glob
import os
import sys

paths = glob.glob("/sys/bus/hid/drivers/razerkbd/0003:1532:022B.*/matrix_custom_frame")
if not paths:
    sys.exit("matrix_custom_frame not found for 1532:022B")
d = os.path.dirname(paths[0])
print("sysfs dir:", d)


def read(attr: str) -> str:
    try:
        with open(os.path.join(d, attr), "rb") as f:
            return f.read().hex(" ") if attr != "device_type" else f.read().decode()
    except OSError as e:
        return f"ERR {e.errno} {e.strerror}"


def write(attr: str, data: bytes) -> str:
    try:
        fd = os.open(os.path.join(d, attr), os.O_WRONLY)
    except OSError as e:
        return f"open ERR {e.errno} {e.strerror}"
    try:
        n = os.write(fd, data)
        return f"OK ({n}/{len(data)} bytes)"
    except OSError as e:
        return f"write ERR {e.errno} {e.strerror}"
    finally:
        os.close(fd)


def step(desc: str, *ops: tuple[str, bytes]) -> None:
    print(f"\n== {desc}")
    for attr, data in ops:
        print(f"  {attr:<22} {data.hex(' ')[:60]:<60} -> {write(attr, data)}")
    input("  What does the pad show? (note it, then Enter) ")


print("device_type:", read("device_type").strip())
print("matrix_brightness (raw):", read("matrix_brightness"))

step("Brightness to max", ("matrix_brightness", b"255"))
step("Static red, whole pad", ("matrix_effect_static", bytes([255, 0, 0])))
step("Effect none (all off)", ("matrix_effect_none", b"1"))

RED, OFF = (255, 0, 0), (0, 0, 0)
for width in (6, 5, 4, 8, 22):
    for row in range(4):
        frame = bytes([row, 0, width - 1, *(RED * width)])
        step(
            f"Custom frame: row {row}, cols 0..{width - 1} red, then apply",
            ("matrix_custom_frame", frame),
            ("matrix_effect_custom", b"1"),
        )
    if input("  Did any row light up at this width? [y/N] ").strip().lower() == "y":
        print(f"\n=> working width: {width}")
        break
    write("matrix_effect_none", b"1")

print("\nAlso paste: sudo dmesg | grep -iE 'razer|hid' | tail -20")
