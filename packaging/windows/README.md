# Windows per-user installer

Run `iroh-share-<version>-windows-x64-setup.exe` from the release. Setup installs
under `%LOCALAPPDATA%\Programs\Iroh Share`, adds Start menu shortcuts, and starts
the daemon in the background for the current account. It registers the daemon to
start at login; administrator access is not required. This is a per-user login
process, not a Windows system service, and does not run before login.

A new desktop configuration is paired automatically using a one-time invitation.
The GUI keeps its own endpoint identity. An existing GUI connection, including a
remote daemon, is preserved. Local folder integration is enabled for the
installer-paired daemon. Closing the GUI leaves the daemon running.

The Start menu includes **Start background daemon** and **Stop background daemon**.
Windows **Settings → Apps → Startup** controls whether it starts at login. The
helper `iroh-share-background.exe --setup-gui` retries initial setup;
`iroh-share-background.exe stop` stops the local daemon gracefully. Logs are
`%LOCALAPPDATA%\iroh-share\daemon.log` and `launcher.log`. Daemon output rotates
to `daemon.previous.log` on startup once the current log exceeds 5 MiB.

Upgrades stop the daemon before replacing its binaries and start it afterwards.
Uninstall stops it and removes the installed files and login entry. Saved daemon
state, GUI identity/settings, and shared/downloaded files remain on disk. The
installer is unsigned, like the standalone binaries.

The release workflow builds the installer with Inno Setup and tests silent
installation, local pairing, sharing, upgrade, uninstall, and preservation of
user data on a Windows runner. Portable ZIP archives remain available.
