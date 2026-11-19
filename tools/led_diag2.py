#!/usr/bin/env python3
"""Second-round lighting diagnostics for the Tartarus V2 (OpenRazer sysfs).

    sudo python3 -I tools/led_diag2.py

After each test, type which keys are lit (e.g. "01-05", "16", "all", "none").
Answers are collected and printed at the end; paste that summary back.
"""
import glob
import os
import sys
import time

paths = glob.glob("/sys/bus/hid/drivers/razerkbd/0003:1532:022B.*/matrix_custom_frame")
if not paths:
    sys.exit("matrix_custom_frame not found for 1532:022B")
d = os.path.dirname(paths[0])

RED, OFF = (255, 0, 0), (0, 0, 0)
results: list[tuple[str, str]] = []


def write(attr: str, data: bytes) -> None:
    fd = os.open(os.path.join(d, attr), os.O_WRONLY)
    try:
        os.write(fd, data)
    except OSError as e:
        print(f"  {attr}: errno {e.errno} {e.strerror}")
    finally:
        os.close(fd)


def row(r: int, cols: int, color) -> bytes:
    return bytes([r, 0, cols - 1, *(color * cols)])


def frame(lit_rows: set[int], rows: int = 4, cols: int = 6, order=None) -> bytes:
    order = order if order is not None else range(rows)
    return b"".join(row(r, cols, RED if r in lit_rows else OFF) for r in order)


def ask(name: str) -> None:
    results.append((name, input(f"[{name}] which keys are lit? ").strip()))


def reset() -> None:
    write("matrix_effect_none", b"1")
    time.sleep(0.2)


def apply(data_writes: list[bytes], delay: float = 0.0, repeat: int = 1) -> None:
    for _ in range(repeat):
        for w in data_writes:
            write("matrix_custom_frame", w)
        time.sleep(delay)
        write("matrix_effect_custom", b"1")
        time.sleep(0.2)


# A: whole frame red, one write, apply
reset(); apply([frame({0, 1, 2, 3})]); ask("A all-red single write")
# B: one row red at a time (full frame, one write)
for r in range(4):
    reset(); apply([frame({r})]); ask(f"B row {r} only")
# C: same as B row 0, but frame written twice (double-buffer test)
reset(); apply([frame({0})], repeat=2); ask("C row 0, written+applied twice")
# D: row 0 only, 100 ms delay between frame and apply
reset(); apply([frame({0})], delay=0.1); ask("D row 0, 100ms delay before apply")
# E: row 0 only, rows sent in reverse order (3,2,1,0)
reset(); apply([frame({0}, order=[3, 2, 1, 0])]); ask("E row 0, reverse row order")
# F: wider rows (8 and 22 cols), all rows red
for cols in (8, 22):
    reset(); apply([frame({0, 1, 2, 3}, cols=cols)]); ask(f"F all red, {cols} cols")
# G: rows 4 and 5 (in case the matrix is taller)
for r in (4, 5):
    reset(); apply([frame({r}, rows=6)]); ask(f"G row {r} only (6-row frame)")

reset()
print("\n--- paste this ---")
for name, ans in results:
    print(f"{name}: {ans}")
