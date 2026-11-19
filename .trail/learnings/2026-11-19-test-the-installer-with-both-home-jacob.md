---
title: "Test the installer with both HOME and XDG_CONFIG_HOME isolated, or it writes to the real config"
author: Jacob
date: 2026-11-19
branch: main
cost: { minutes: 3, tokens: 36528 }
id: l-71945464
scope: repo
status: active
src: agent
touches:
  - CLAUDE.md
  - DESIGN.md
  - README.md
  - packaging/install.sh
  - packaging/config.example.toml
  - docs/examples/smoke-test.toml
  - crates/core/src/config.rs
  - crates/core/src/lib.rs
  - crates/core/src/machine.rs
  - crates/daemon/src/app.rs
---

## Decided
- The installer honors XDG_CONFIG_HOME (as it should) for the user unit and ~/.config/tartarusd, and uses HOME only for the binary prefix. It never uses root and never starts the service.

## Tried and ruled out
- Testing it with only HOME pointed at a scratch dir: on a machine where XDG_CONFIG_HOME is already set (this one: /home/simple/.config), the config and a systemd unit were written into the real config dir, with ExecStart pointing at the scratch path. This happened twice, the second time because a zsh variable holding two assignments is not word-split.

## Watch out
- Set both variables inline and assert the path before running, for example: HOME=$S/h XDG_CONFIG_HOME=$S/h/.config sh packaging/install.sh --no-build, after checking that "${XDG_CONFIG_HOME:-$HOME/.config}" resolves inside $S.
- In zsh, do not stash "A=1 B=2" in a variable and expand it after env; write the assignments inline.
- Validate the result with: systemd-analyze --user verify <unit>, and udevadm verify packaging/70-tartarusd.rules.

In the code:
- packaging/install.sh (flags: --features NAME, --no-build), packaging/tartarusd.service, packaging/70-tartarusd.rules.
