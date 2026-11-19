#!/bin/sh
# Install tartarusd for the current user (no root needed).
#
#   packaging/install.sh                     build and install
#   packaging/install.sh --features hyprland also build window-based profile switching
#   packaging/install.sh --gui               also build and install the GTK4 editor (needs GTK4 and
#                                            libadwaita development packages)
#   packaging/install.sh --no-build          install the binaries already in target/release
#
# Installs to $PREFIX/bin (default ~/.local/bin) and the user systemd unit. It never
# touches /etc or starts anything; the udev rule needs root and is printed at the end.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
prefix=${PREFIX:-$HOME/.local}
bin=$prefix/bin
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
conf_dir=${XDG_CONFIG_HOME:-$HOME/.config}/tartarusd

build=1
gui=0
features=
while [ $# -gt 0 ]; do
    case $1 in
        --no-build) build=0 ;;
        --gui) gui=1 ;;
        --features)
            [ $# -ge 2 ] || { echo "--features needs a value" >&2; exit 2; }
            features=$2
            shift
            ;;
        -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

cd "$here"
pkgs="-p tartarusd -p tartarusctl"
bins="tartarusd tartarusctl"
if [ "$gui" = 1 ]; then
    pkgs="$pkgs -p tartarusd-gui"
    bins="$bins tartarusd-gui"
fi
if [ "$build" = 1 ]; then
    if [ -n "$features" ]; then
        # shellcheck disable=SC2086
        cargo build --release $pkgs --features "tartarusd/$features"
    else
        # shellcheck disable=SC2086
        cargo build --release $pkgs
    fi
fi
for b in $bins; do
    [ -x "target/release/$b" ] || { echo "target/release/$b not found; build first" >&2; exit 1; }
done

install -Dm755 target/release/tartarusd "$bin/tartarusd"
install -Dm755 target/release/tartarusctl "$bin/tartarusctl"
if [ "$gui" = 1 ]; then
    install -Dm755 target/release/tartarusd-gui "$bin/tartarusd-gui"
    apps=${XDG_DATA_HOME:-$HOME/.local/share}/applications
    mkdir -p "$apps"
    sed "s|^Exec=.*|Exec=$bin/tartarusd-gui|" packaging/tartarusd-gui.desktop > "$apps/org.tartarusd.Gui.desktop"
fi

# The unit's ExecStart assumes ~/.local/bin; point it at the real location.
mkdir -p "$unit_dir"
sed "s|^ExecStart=.*|ExecStart=$bin/tartarusd|" packaging/tartarusd.service > "$unit_dir/tartarusd.service"

mkdir -p "$conf_dir/profiles"
if [ ! -e "$conf_dir/config.toml" ]; then
    install -m644 packaging/config.example.toml "$conf_dir/config.toml"
    echo "created $conf_dir/config.toml"
fi

cat <<MSG

Installed:
  $bin/tartarusd, $bin/tartarusctl$([ "$gui" = 1 ] && echo ", $bin/tartarusd-gui")
  $unit_dir/tartarusd.service
  $conf_dir/  (config.toml, profiles/)

Next steps:
  1. Allow your user to open the device and /dev/uinput (needs root, once):
       sudo install -Dm644 packaging/70-tartarusd.rules /etc/udev/rules.d/70-tartarusd.rules
       sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=input --subsystem-match=misc
  2. Put at least one profile in $conf_dir/profiles/ (see docs/examples/).
  3. Make sure openrazer-daemon is running and input-remapper is not grabbing the Tartarus.
  4. systemctl --user daemon-reload
     systemctl --user enable --now tartarusd.service
MSG
