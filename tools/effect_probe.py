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
STEPS = [
    ("static red", "setStatic", "yyy", (255, 0, 0), ""),
    ("spectrum (rainbow cycle)", "setSpectrum", "", (), ""),
    ("wave, direction 1", "setWave", "i", (1,), "which way does it travel?"),
    ("wave, direction 2", "setWave", "i", (2,), "the opposite way?"),
    ("breathing, single cyan", "setBreathSingle", "yyy", (0, 255, 255), ""),
    ("breathing, dual red/blue", "setBreathDual", "yyyyyy", (255, 0, 0, 0, 0, 255), ""),
    ("breathing, random", "setBreathRandom", "", (), ""),
    ("reactive, speed 2, green", "setReactive", "yyyy", (2, 0, 255, 0), "PRESS KEYS on the pad now"),
    ("starlight single, speed 2, magenta", "setStarlightSingle", "yyyy", (2, 255, 0, 255), ""),
    ("starlight dual, speed 2, red/blue", "setStarlightDual", "yyyyyyy", (2, 255, 0, 0, 0, 0, 255), ""),
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
