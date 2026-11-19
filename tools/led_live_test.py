#!/usr/bin/env python3
"""Does a new custom frame display without effect_none in between?

    sudo python3 -I tools/led_live_test.py
"""
import glob
import os
import sys
import time

paths = glob.glob("/sys/bus/hid/drivers/razerkbd/0003:1532:022B.*/matrix_custom_frame")
if not paths:
    sys.exit("matrix_custom_frame not found for 1532:022B")
d = os.path.dirname(paths[0])
RED, GREEN, OFF = (255, 0, 0), (0, 255, 0), (0, 0, 0)


def write(attr: str, data: bytes) -> None:
    fd = os.open(os.path.join(d, attr), os.O_WRONLY)
    try:
        os.write(fd, data)
    except OSError as e:
        print(f"  {attr}: errno {e.errno} {e.strerror}", file=sys.stderr)
    finally:
        os.close(fd)


def frame(lit_row: int, color) -> bytes:
    return b"".join(
        bytes([r, 0, 5, *((color if r == lit_row else OFF) * 6)]) for r in range(4)
    )


def custom(data: bytes) -> None:
    write("matrix_custom_frame", data)
    write("matrix_effect_custom", b"1")
    time.sleep(0.2)


results = []
write("matrix_effect_none", b"1")
time.sleep(0.2)
custom(frame(0, RED))
results.append(("H1 row 0 red (after reset)", input("[H1] What is lit? ")))

custom(frame(1, GREEN))
results.append(("H2 row 1 green, no reset", input("[H2] What is lit? ")))

write("matrix_custom_frame", frame(2, RED))
time.sleep(0.2)
results.append(("H3 row 2 red, frame only (no effect_custom)", input("[H3] What is lit? ")))

write("matrix_effect_none", b"1")
print("\n--- paste this ---")
for name, ans in results:
    print(f"{name}: {ans}")
