# Synology package

Releases include `iroh-share-<version>-dsm6.spk` and `-dsm7.spk`, built by
`build.py` from the static Linux release archives. Use the one matching your
DSM version: DSM 7 refuses packages built for DSM 6. Both contain binaries for
x86_64 and aarch64 (`noarch`); 32-bit ARM models are not supported.

## Install

In Package Center choose **Manual Install** and select the `.spk`. On DSM 6,
first allow unsigned packages under Package Center → Settings → General →
Trust Level → Any publisher. Or over SSH:

```sh
sudo synopkg install iroh-share-<version>-dsm7.spk
sudo synopkg start iroh-share
```

Package Center starts the daemon at boot. It does not restart it after a crash.

## Data and logs

- DSM 7: `/var/packages/iroh-share/var`, run as the package user `iroh-share`.
  To share folders, give that user read access to them.
- DSM 6: `/var/packages/iroh-share/target/var`, run as root. Upgrades keep it;
  uninstalling removes it.

The state is in `state/`, the log in `daemon.log` (the previous run in
`daemon.previous.log`), and ticket imports in `Downloads/Iroh Share`. To change
the log filter, put a `RUST_LOG` value such as `info,iroh_blobs::provider=debug`
in `rust_log` and restart the package.

## Pairing a GUI or TUI

Package Center's log view for the package (View Log) starts with a fresh
single-use pairing ticket, followed by the end of `daemon.log`. Each view makes
a new ticket. The ticket is never written to `daemon.log`.

## Using the CLI

`bin/iroh-share` runs the right binary with the package's state, so CLI
commands reach the running daemon. On DSM 7, run it as the package user:

```sh
sudo -u iroh-share /var/packages/iroh-share/target/bin/iroh-share list
# Pair a TUI or GUI on another machine:
sudo -u iroh-share /var/packages/iroh-share/target/bin/iroh-share control pair
```

On DSM 6, use `sudo` without `-u iroh-share`.

## Building

```sh
python3 packaging/synology/build.py dist   # needs both *-linux-musl.tar.gz in dist/
python3 -m unittest discover -s packaging/synology
```
