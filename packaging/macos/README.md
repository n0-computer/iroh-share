# macOS per-user installer

On Apple Silicon with macOS 13 or later, open
`iroh-share-<version>-macos-arm64.pkg`. It installs `Iroh Share.app` in
`~/Applications`, registers a per-user LaunchAgent, starts the daemon, and pairs a
new GUI configuration. Install for your own account, without `sudo`. Existing GUI
connections are preserved.

The daemon starts at login and stays running after the GUI closes. launchd
restarts failed daemon exits; an explicit graceful stop leaves it stopped until
login or restart. The daemon handles SIGTERM for logout and service shutdown.
The LaunchAgent is `~/Library/LaunchAgents/computer.n0.iroh-share.plist`; state
and logs live in `~/Library/Application Support/iroh-share`.

macOS can ask you to approve background activity and access to protected folders.
Installation does not bypass Downloads/Documents/Desktop privacy permissions.
The app has an ad-hoc signature for bundle integrity; the package is not Developer
ID signed or notarized, so this build still requires explicit approval under
macOS download protections.

To remove it, close the GUI and run `~/Applications/Uninstall Iroh Share.command`.
It stops and unregisters the LaunchAgent, then removes the app and uninstaller.
Saved state, GUI identity/settings, and shared/downloaded files are preserved.
