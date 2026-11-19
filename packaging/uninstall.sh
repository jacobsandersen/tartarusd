#!/bin/sh
# Remove what packaging/install.sh installed for the current user.
#
#   packaging/uninstall.sh
#
# Stops and disables the user service, then deletes the binaries, unit, desktop entry and
# icons. Your config and profiles in ~/.config/tartarusd are kept. Uses the same PREFIX,
# XDG_CONFIG_HOME and XDG_DATA_HOME as install.sh.
set -eu

prefix=${PREFIX:-$HOME/.local}
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
data=${XDG_DATA_HOME:-$HOME/.local/share}
conf_dir=${XDG_CONFIG_HOME:-$HOME/.config}/tartarusd

if [ -f "$unit_dir/tartarusd.service" ] && command -v systemctl >/dev/null 2>&1; then
    systemctl --user disable --now tartarusd.service 2>/dev/null || true
fi

for f in \
    "$prefix/bin/tartarusd" \
    "$prefix/bin/tartarusctl" \
    "$prefix/bin/tartarusd-gui" \
    "$unit_dir/tartarusd.service" \
    "$data/applications/org.tartarusd.Gui.desktop"
do
    if [ -e "$f" ]; then
        rm -f "$f"
        echo "removed $f"
    fi
done

for size in 48 64 128 256 512; do
    f="$data/icons/hicolor/${size}x${size}/apps/tartarusd.png"
    if [ -e "$f" ]; then
        rm -f "$f"
        echo "removed $f"
    fi
done

if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload 2>/dev/null || true
fi

echo
echo "Kept: $conf_dir (config and profiles)"
if [ -e /etc/udev/rules.d/70-tartarusd.rules ]; then
    echo "An old udev rule is still in /etc (the Arch package installs its own). Remove it with:"
    echo "  sudo rm /etc/udev/rules.d/70-tartarusd.rules && sudo udevadm control --reload"
fi
