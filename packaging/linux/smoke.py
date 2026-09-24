"""Exercise per-user archive installation on an ephemeral Linux CI runner."""
import os
import pathlib
import subprocess
import tarfile
import tempfile
import time

if os.environ.get("CI") != "true":
    raise SystemExit("Run this installation test only on an ephemeral CI runner")
root = pathlib.Path(__file__).resolve().parents[2]
archive = next((root / "dist").glob("blobtorrent-*-linux-gnu.tar.gz"))
home = pathlib.Path.home()
install_dir = home / ".local/share/blobtorrent"
bin_dir = home / ".local/bin"
state = home / ".local/state/blobtorrent"
config = home / ".config/blobtorrent-gui"
unit = home / ".config/systemd/user/blobtorrent.service"
entry = home / ".local/share/applications/blobtorrent.desktop"
logs = root / "installer-test-logs"
logs.mkdir(exist_ok=True)

# The runner process is not a login session; talk to the lingering user manager.
runtime = pathlib.Path(f"/run/user/{os.getuid()}")
os.environ.setdefault("XDG_RUNTIME_DIR", str(runtime))
os.environ.setdefault("DBUS_SESSION_BUS_ADDRESS", f"unix:path={runtime / 'bus'}")
deadline = time.monotonic() + 30
while not (runtime / "bus").exists():
    assert time.monotonic() < deadline, "systemd user manager did not start; run loginctl enable-linger"
    time.sleep(0.5)

def run(label, command, timeout=180):
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    (logs / f"{label}.log").write_text(result.stdout + result.stderr)
    assert result.returncode == 0, result.stdout + result.stderr
    return result.stdout

def cli(*args):
    return subprocess.run([str(bin_dir / "blobtorrent"), *map(str, args)],
                          check=True, capture_output=True, text=True, timeout=30).stdout

def active():
    return subprocess.run(["systemctl", "--user", "is-active", "blobtorrent.service"],
                          capture_output=True, text=True).stdout.strip()

with tempfile.TemporaryDirectory() as temporary:
    with tarfile.open(archive) as tar:
        tar.extractall(temporary, filter="data")
    unpacked = next(pathlib.Path(temporary).iterdir())
    try:
        # A missing --yes must refuse to install when stdin is not a terminal.
        refused = subprocess.run(["sh", str(unpacked / "install.sh")], stdin=subprocess.DEVNULL,
                                 capture_output=True, text=True, timeout=60)
        assert refused.returncode != 0 and not unit.exists(), "installer must ask before changing anything"
        run("install", ["sh", str(unpacked / "install.sh"), "--yes"])
        assert unit.is_file(), "systemd user unit missing"
        assert entry.is_file(), "desktop entry missing"
        assert active() == "active", f"service is {active()}"
        for binary in ("blobtorrent", "blobtorrent-background", "blobtorrent-gui", "blobtorrent-tui"):
            assert (bin_dir / binary).resolve() == install_dir / "bin" / binary
        cli("list")
        identity = (config / "control-client.key").read_bytes()
        connection = (config / "client.json").read_bytes()
        assert (config / "local-endpoint").is_file()
        source = home / "blobtorrent-installer-test.txt"
        source.write_text("Installer lifecycle check")
        cli("share", source)
        deadline = time.monotonic() + 30
        while "Seeding" not in cli("list"):
            assert time.monotonic() < deadline, "Share did not finish"
            time.sleep(0.25)
        run("upgrade", ["sh", str(unpacked / "install.sh"), "--yes"])
        assert active() == "active", f"service is {active()} after upgrade"
        assert (config / "control-client.key").read_bytes() == identity
        assert (config / "client.json").read_bytes() == connection
        assert "Seeding" in cli("list"), "Upgrade lost data"
        run("uninstall", ["sh", str(install_dir / "uninstall.sh"), "--yes"], timeout=90)
        assert not install_dir.exists()
        assert not unit.exists() and not entry.exists()
        assert not (bin_dir / "blobtorrent").exists()
        assert active() != "active"
        assert not (state / "control.addr").exists()
        assert source.is_file() and (state / "names.json").is_file()
        assert (config / "control-client.key").read_bytes() == identity
        print("PASS: per-user archive, systemd user service, pairing, share, upgrade, uninstall, data retention")
    finally:
        for name in ("daemon.log", "launcher.log"):
            file = state / name
            if file.exists():
                (logs / name).write_bytes(file.read_bytes())
        subprocess.run(["journalctl", "--user", "-u", "blobtorrent.service", "--no-pager"],
                       stdout=(logs / "journal.log").open("w"), stderr=subprocess.STDOUT)
