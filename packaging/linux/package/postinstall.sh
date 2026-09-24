#!/bin/sh
# Enable the per-user service for every account. It starts at the next login.
set -e
if command -v systemctl >/dev/null 2>&1; then
    systemctl --global enable blobtorrent.service >/dev/null 2>&1 || true
fi
echo 'Blobtorrent: the daemon starts at each user login. Start it now with'
echo '  systemctl --user start blobtorrent.service'
echo 'and pair the desktop app once with: blobtorrent-background --setup-gui'
