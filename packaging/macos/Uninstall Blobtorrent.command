#!/bin/sh
set -eu
if [ "$(id -u)" -eq 0 ]; then
    echo 'Run this uninstaller as your own user, without sudo.' >&2
    exit 1
fi
if [ "${1:-}" != '--yes' ]; then
    printf 'Close Blobtorrent before continuing. Remove the app and background login agent? Your saved data and files will be kept. [y/N] '
    read -r answer
    case "$answer" in y|Y|yes|YES) ;; *) exit 0 ;; esac
fi
app="$HOME/Applications/Blobtorrent.app"
"$app/Contents/MacOS/blobtorrent-background" remove-agent
# Remove only the installed app and its own uninstaller, never user data.
/bin/rm -rf "$app"
/bin/rm -f "$HOME/Applications/Uninstall Blobtorrent.command"
printf 'Blobtorrent has been uninstalled. Your data and settings have been kept.\n'
