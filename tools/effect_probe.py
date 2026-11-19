#!/usr/bin/env python3
"""Step through the firmware lighting effects OpenRazer offers for the Tartarus V2.

Watch the pad. Each effect runs for a few seconds and the script prints what it set and
what OpenRazer reports back, so you can say which ones looked right. It only calls the
OpenRazer D-Bus API (no root) and finishes with restoreLastEffect.

Stop tartarusd first so they don't fight over the pad:
    systemctl --user stop tartarusd.service
    python3 tools/effect_probe.py
    systemctl --user start tartarusd.service

Optional argument: seconds per effect (default 6).
"""

import subprocess
import sys
import time

BUS = "org.razer"
CHROMA = "razer.device.lighting.chroma"
PID = "1532:022B"


def busctl(*args: str) -> str:
    out = subprocess.run(
        ["busctl", "--user", *args], capture_output=True, text=True, check=False
    )
    return (out.stdout or out.stderr).strip()


def find_device() -> str:
    serials = [
        s.strip('"')
        for s in busctl("call", BUS, "/org/razer", "razer.devices", "getDevices").split()[2:]
    ]
    for s in serials:
        path = f"/org/razer/device/{s}"
        name = busctl("call", BUS, path, "razer.device.misc", "getDeviceName")
        if "Tartarus" in name:
            return path
    sys.exit("No Tartarus found on org.razer (is openrazer-daemon running?)")


def call(path: str, method: str, sig: str = "", *vals: int) -> str:
    return busctl("call", BUS, path, CHROMA, method, *([sig, *map(str, vals)] if sig else []))


def readback(path: str) -> str:
    parts = []
    for m in ("getEffect", "getEffectColors", "getEffectSpeed", "getWaveDir"):
        parts.append(f"{m}={call(path, m)!s}")
    return "  ".join(parts)


# (label, method, signature, values, note)
# Argument order is from OpenRazer's own endpoints: colors first, speed LAST.
STEPS = [
    ("static red", "setStatic", "yyy", (255, 0, 0), ""),
    ("spectrum (rainbow cycle)", "setSpectrum", "", (), ""),
    ("wave, direction 1", "setWave", "i", (1,), "which way does it travel?"),
    ("wave, direction 2", "setWave", "i", (2,), "the opposite way?"),
    ("breathing, single cyan", "setBreathSingle", "yyy", (0, 255, 255), ""),
    ("breathing, dual red/blue", "setBreathDual", "yyyyyy", (255, 0, 0, 0, 0, 255), "red then blue?"),
    ("breathing, random", "setBreathRandom", "", (), ""),
    ("reactive RED, speed 1", "setReactive", "yyyy", (255, 0, 0, 1), "PRESS KEYS: color? how long does the glow last?"),
    ("reactive RED, speed 4", "setReactive", "yyyy", (255, 0, 0, 4), "PRESS KEYS: longer or shorter than speed 1?"),
    ("starlight single MAGENTA, speed 1", "setStarlightSingle", "yyyy", (255, 0, 255, 1), "color? twinkle rate?"),
    ("starlight single MAGENTA, speed 3", "setStarlightSingle", "yyyy", (255, 0, 255, 3), "faster or slower than speed 1?"),
    ("starlight dual red/blue, speed 2", "setStarlightDual", "yyyyyyy", (255, 0, 0, 0, 0, 255, 2), "red and blue?"),
    ("starlight random, speed 2", "setStarlightRandom", "y", (2,), ""),
    ("none (all off)", "setNone", "", (), ""),
]


def main() -> None:
    seconds = float(sys.argv[1]) if len(sys.argv) > 1 else 6.0
    if "org.tartarusd.Daemon1" in busctl("--no-pager", "list"):
        print("WARNING: tartarusd is running; stop it first (see the top of this file).\n")
    path = find_device()
    print(f"device: {path}\nWatch the pad. {seconds:.0f} s per effect.\n")
    time.sleep(3)
    for i, (label, method, sig, vals, note) in enumerate(STEPS, 1):
        print(f"[{i}/{len(STEPS)}] {label}" + (f"   <-- {note}" if note else ""), flush=True)
        print(f"    {method} {sig} {' '.join(map(str, vals))}".rstrip())
        reply = call(path, method, sig, *vals)
        if reply:
            print(f"    reply: {reply}")
        print(f"    {readback(path)}", flush=True)
        time.sleep(seconds)
    print("\nrestoreLastEffect")
    print(call(path, "restoreLastEffect") or "ok")


if __name__ == "__main__":
    main()
