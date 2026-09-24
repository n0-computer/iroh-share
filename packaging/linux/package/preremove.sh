#!/bin/sh
# Disable the per-user service on final removal only. deb passes "upgrade"
# and rpm passes 1 when a newer version replaces this one; pacman runs
# pre_upgrade instead of pre_remove, so any other argument means removal.
set -e
case "${1:-}" in
    upgrade|1) ;;
    *)
        if command -v systemctl >/dev/null 2>&1; then
            systemctl --global disable blobtorrent.service >/dev/null 2>&1 || true
        fi ;;
esac
