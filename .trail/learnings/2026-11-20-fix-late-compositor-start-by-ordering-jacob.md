---
title: "Fix late compositor start by ordering the service after the graphical session, not by retrying detection"
author: Jacob
date: 2026-11-20
branch: fix/start-after-graphical-session
cost: { minutes: 6, tokens: 77632 }
id: l-a2f31713
scope: repo
status: active
src: agent
touches:
  - README.md
  - packaging/tartarusd.service
  - crates/daemon/src/window/mod.rs
---

## Decided
Window auto-switching failed after a fresh boot because the user service started before Hyprland had created its sockets, and the daemon detects the compositor only once. The fix is in the unit: start with the graphical session, after the compositor is up, and still after OpenRazer. The maintainer prefers this over making the daemon cope.

## Tried and ruled out
Retrying Hyprland detection every 2s inside the daemon. It worked and passed tests, but the maintainer rejected it: startup timing is the unit's job, and the daemon should keep its simple one-shot detection.

## Watch out
- An install already enabled under default.target keeps its old symlink until `systemctl --user reenable tartarusd.service`.
- A compositor started without a session manager (no uwsm) never reaches graphical-session.target, so the service would not start; the README troubleshooting row covers the workaround.
- Symptom to recognise: log line "hyprland backend compiled in but Hyprland was not detected" right after boot.

In the code: packaging/tartarusd.service (PartOf/After/WantedBy graphical-session.target), crates/daemon/src/window/mod.rs (one-shot detect), README.md troubleshooting.
