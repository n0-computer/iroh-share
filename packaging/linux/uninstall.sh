#!/bin/sh
# Remove a per-user Blobtorrent installation. Shared files and settings stay.
set -eu

install_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
bin_dir=${BLOBTORRENT_BIN_DIR:-$HOME/.local/bin}
helper="$install_dir/bin/blobtorrent-background"

if [ "$(id -u)" -eq 0 ]; then
    echo 'Run this uninstaller as your own user, without sudo.' >&2
    exit 1
fi
if [ ! -x "$helper" ]; then
    echo "uninstall.sh: $helper not found; is this the installed copy?" >&2
    exit 1
fi
if [ "${1:-}" != '--yes' ]; then
    printf 'Close Blobtorrent before continuing. Remove the programs and the systemd user service? Your saved data and files will be kept. [y/N] '
    read -r answer
    case "$answer" in y|Y|yes|YES) ;; *) exit 0 ;; esac
fi

if [ -n "${BLOBTORRENT_STATE_DIR:-}" ]; then
    "$helper" --state-dir "$BLOBTORRENT_STATE_DIR" remove-agent
else
    "$helper" remove-agent
fi
for binary in blobtorrent blobtorrent-background blobtorrent-gui blobtorrent-tui; do
    link="$bin_dir/$binary"
    # Remove only links that point into this installation, never other programs.
    if [ -L "$link" ] && [ "$(readlink "$link")" = "$install_dir/bin/$binary" ]; then
        rm -f "$link"
    fi
done
# Remove only the installed programs and this uninstaller, never user data.
rm -rf "$install_dir"
printf 'Blobtorrent has been uninstalled. Your data and settings have been kept.\n'
