"""Exercise per-user package installation on an ephemeral macOS CI runner."""
import os
import pathlib
import subprocess
import time

if os.environ.get("CI") != "true":
    raise SystemExit("Run this installation test only on an ephemeral CI runner")
root = pathlib.Path(__file__).resolve().parents[2]
package = next((root / "dist").glob("*-macos-arm64.pkg"))
home = pathlib.Path.home()
app = home / "Applications/Iroh Share.app"
state = home / "Library/Application Support/iroh-share"
config = home / "Library/Application Support/iroh-share-gui"
agent = home / "Library/LaunchAgents/computer.n0.iroh-share.plist"
logs = root / "installer-test-logs"
logs.mkdir(exist_ok=True)

def install(label):
    result = subprocess.run(["/usr/sbin/installer", "-pkg", str(package), "-target", "CurrentUserHomeDirectory"],
                            capture_output=True, text=True, timeout=180)
    (logs / f"{label}.log").write_text(result.stdout + result.stderr)
    assert result.returncode == 0, result.stdout + result.stderr

def cli(*args):
    return subprocess.run([str(app / "Contents/MacOS/iroh-share"), *map(str, args)],
                          check=True, capture_output=True, text=True, timeout=30).stdout

try:
    install("install")
    assert agent.is_file(), "LaunchAgent missing"
    assert app.stat().st_uid == os.getuid(), "App must be owned by installing user"
    cli("list")
    identity = (config / "control-client.key").read_bytes()
    connection = (config / "client.json").read_bytes()
    assert (config / "local-endpoint").is_file()
    source = home / "iroh-share-installer-test.txt"
    source.write_text("Installer lifecycle check")
    cli("share", source)
    deadline = time.monotonic() + 30
    while "Seeding" not in cli("list"):
        assert time.monotonic() < deadline, "Share did not finish"
        time.sleep(0.25)
    install("upgrade")
    assert (config / "control-client.key").read_bytes() == identity
    assert (config / "client.json").read_bytes() == connection
    assert "Seeding" in cli("list"), "Upgrade lost data"
    result = subprocess.run(["/bin/sh", str(home / "Applications/Uninstall Iroh Share.command"), "--yes"],
                            capture_output=True, text=True, timeout=90)
    (logs / "uninstall.log").write_text(result.stdout + result.stderr)
    assert result.returncode == 0, result.stdout + result.stderr
    assert not app.exists()
    assert not agent.exists()
    assert not (state / "control.addr").exists()
    assert source.is_file() and (state / "names.json").is_file()
    assert (config / "control-client.key").read_bytes() == identity
    print("PASS: per-user package, LaunchAgent, pairing, share, upgrade, uninstall, data retention")
finally:
    for name in ("daemon.log", "launcher.log"):
        file = state / name
        if file.exists():
            (logs / name).write_bytes(file.read_bytes())
