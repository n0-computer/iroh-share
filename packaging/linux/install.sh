#!/bin/sh
# Per-user installer for the Blobtorrent Linux archive. Read README first.
set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
data_home=${XDG_DATA_HOME:-$HOME/.local/share}
install_dir=${BLOBTORRENT_INSTALL_DIR:-$data_home/blobtorrent}
bin_dir=${BLOBTORRENT_BIN_DIR:-$HOME/.local/bin}
state_dir=${BLOBTORRENT_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/blobtorrent}
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
binaries="blobtorrent blobtorrent-background blobtorrent-gui blobtorrent-tui"

confirm=yes
for argument in "$@"; do
    case "$argument" in
        --yes|-y) confirm=no ;;
        -h|--help)
            echo "usage: install.sh [--yes]"
            echo "Installs Blobtorrent for the current user. --yes skips the confirmation."
            exit 0 ;;
        *) echo "install.sh: unknown argument: $argument" >&2; exit 2 ;;
    esac
done

if [ "$(id -u)" -eq 0 ]; then
    echo 'Run this installer as your own user, without sudo.' >&2
    exit 1
fi
for binary in $binaries; do
    if [ ! -f "$here/$binary" ]; then
        echo "install.sh: $binary is missing; run this script from the unpacked archive." >&2
        exit 1
    fi
done
if ! systemctl --user show --property=Version >/dev/null 2>&1; then
    echo 'No systemd user session is available for this account.' >&2
    echo 'Log in through a graphical or systemd-managed session, or run: loginctl enable-linger' >&2
    exit 1
fi

cat <<SUMMARY
Blobtorrent installer

This installs Blobtorrent for your user account only. No sudo is needed.
It will:

  1. Copy blobtorrent, blobtorrent-background, blobtorrent-gui and
     blobtorrent-tui to
       $install_dir/bin
  2. Link those programs into
       $bin_dir
  3. Write a systemd user service, blobtorrent.service, to
       $config_home/systemd/user
     enable it so the daemon starts when you log in, and start it now.
  4. Add "Blobtorrent" to your application menu.
  5. Pair the desktop app with the daemon, unless it is already configured.

Shared files and daemon state live in
  $state_dir
An existing installation is stopped and replaced. Your data is kept.
Uninstall later with: $install_dir/uninstall.sh

SUMMARY
if [ "$confirm" = yes ]; then
    if [ ! -t 0 ]; then
        echo 'Standard input is not a terminal. Rerun with --yes to install without confirmation.' >&2
        exit 1
    fi
    printf 'Continue? [y/N] '
    read -r answer
    case "$answer" in
        y|Y|yes|YES) ;;
        *) echo 'Installation cancelled. Nothing was changed.'; exit 0 ;;
    esac
fi

run_helper() {
    if [ -n "${BLOBTORRENT_GUI_CONFIG_DIR:-}" ]; then
        set -- --gui-config-dir "$BLOBTORRENT_GUI_CONFIG_DIR" "$@"
    fi
    if [ -n "${BLOBTORRENT_STATE_DIR:-}" ]; then
        set -- --state-dir "$BLOBTORRENT_STATE_DIR" "$@"
    fi
    "$helper" "$@"
}

# Stop a daemon from a previous installation before its files are replaced.
helper="$install_dir/bin/blobtorrent-background"
if [ -x "$helper" ]; then
    echo 'Stopping the running Blobtorrent daemon...'
    if ! run_helper stop; then
        echo "The background daemon could not be stopped. Close it and retry. Details are in $state_dir/launcher.log" >&2
        exit 1
    fi
fi

echo "Installing programs to $install_dir/bin"
mkdir -p "$install_dir/bin" "$bin_dir"
for binary in $binaries; do
    cp "$here/$binary" "$install_dir/bin/.$binary.new"
    chmod 755 "$install_dir/bin/.$binary.new"
    mv -f "$install_dir/bin/.$binary.new" "$install_dir/bin/$binary"
done
cp "$here/uninstall.sh" "$install_dir/uninstall.sh"
chmod 755 "$install_dir/uninstall.sh"
cp "$here/README" "$install_dir/README"
if [ -d "$here/docs" ]; then
    rm -rf "$install_dir/docs"
    cp -R "$here/docs" "$install_dir/docs"
fi
for binary in $binaries; do
    ln -sfn "$install_dir/bin/$binary" "$bin_dir/$binary"
done

echo 'Registering and starting the systemd user service...'
if ! run_helper install-agent; then
    echo "Blobtorrent is copied, but the background service could not be set up. See $state_dir/launcher.log, then rerun install.sh." >&2
    exit 1
fi

echo
echo 'Blobtorrent is installed and the daemon is running.'
echo '  Desktop app:  blobtorrent-gui   (also in your application menu)'
echo '  Terminal UI:  blobtorrent-tui'
echo '  Command line: blobtorrent list'
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) echo "Note: $bin_dir is not in your PATH. Add it, or run the programs by full path." ;;
esac
