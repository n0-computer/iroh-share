"""Exercise the deb on the Ubuntu CI runner, and the rpm and Arch packages in containers."""
import os
import pathlib
import platform
import subprocess
import time

if os.environ.get("CI") != "true":
    raise SystemExit("Run this installation test only on an ephemeral CI runner")
root = pathlib.Path(__file__).resolve().parents[2]
dist = root / "dist"
deb = next(dist.glob("blobtorrent_*.deb"))
rpm = next(dist.glob("blobtorrent-*.rpm"))
pacman = next(dist.glob("blobtorrent-*.pkg.tar.zst"))
home = pathlib.Path.home()
state = home / ".local/state/blobtorrent"
config = home / ".config/blobtorrent-gui"
wants = pathlib.Path("/etc/systemd/user/default.target.wants/blobtorrent.service")
logs = root / "installer-test-logs"
logs.mkdir(exist_ok=True)

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
    return subprocess.run(["/usr/bin/blobtorrent", *map(str, args)],
                          check=True, capture_output=True, text=True, timeout=30).stdout

def active():
    return subprocess.run(["systemctl", "--user", "is-active", "blobtorrent.service"],
                          capture_output=True, text=True).stdout.strip()

try:
    run("deb-install", ["sudo", "apt-get", "install", "-y", str(deb)])
    for path in ("/usr/bin/blobtorrent", "/usr/bin/blobtorrent-background", "/usr/bin/blobtorrent-gui",
                 "/usr/bin/blobtorrent-tui", "/usr/lib/systemd/user/blobtorrent.service",
                 "/usr/share/applications/blobtorrent.desktop"):
        assert pathlib.Path(path).is_file(), f"{path} missing"
    assert wants.is_symlink(), "service is not enabled globally"
    run("deb-start", ["systemctl", "--user", "start", "blobtorrent.service"])
    assert active() == "active", f"service is {active()}"
    cli("list")
    run("deb-setup-gui", ["/usr/bin/blobtorrent-background", "--setup-gui"])
    assert (config / "client.json").is_file() and (config / "local-endpoint").is_file()
    identity = (config / "control-client.key").read_bytes()
    source = home / "blobtorrent-package-test.txt"
    source.write_text("Package lifecycle check")
    cli("share", source)
    deadline = time.monotonic() + 30
    while "Seeding" not in cli("list"):
        assert time.monotonic() < deadline, "Share did not finish"
        time.sleep(0.25)
    run("deb-upgrade", ["sudo", "apt-get", "install", "-y", "--reinstall", str(deb)])
    assert wants.is_symlink(), "upgrade disabled the service"
    assert active() == "active", f"service is {active()} after upgrade"
    run("deb-stop", ["systemctl", "--user", "stop", "blobtorrent.service"])
    run("deb-remove", ["sudo", "apt-get", "remove", "-y", "blobtorrent"])
    assert not pathlib.Path("/usr/bin/blobtorrent").exists()
    assert not pathlib.Path("/usr/lib/systemd/user/blobtorrent.service").exists()
    assert not wants.exists(), "removal left the service enabled"
    assert source.is_file() and (state / "names.json").is_file()
    assert (config / "control-client.key").read_bytes() == identity
    run("rpm-install", ["docker", "run", "--rm", "-v", f"{dist}:/dist:ro", "fedora:latest", "sh", "-ec",
                        "dnf install -y systemd >/dev/null"  # the base image has no systemctl for the scriptlets
                        f" && dnf install -y /dist/{rpm.name} && rpm -q blobtorrent && blobtorrent --help"
                        " && test -f /usr/lib/systemd/user/blobtorrent.service"
                        " && test -L /etc/systemd/user/default.target.wants/blobtorrent.service"
                        " && dnf remove -y blobtorrent && test ! -e /usr/bin/blobtorrent"], timeout=600)
    # The official Arch Linux image is x86_64 only.
    if platform.machine() == "x86_64":
        run("pacman-install", ["docker", "run", "--rm", "-v", f"{dist}:/dist:ro", "archlinux:latest", "sh", "-ec",
                               f"pacman -U --noconfirm /dist/{pacman.name} && pacman -Qi blobtorrent && blobtorrent --help"
                               " && test -f /usr/lib/systemd/user/blobtorrent.service"
                               " && test -L /etc/systemd/user/default.target.wants/blobtorrent.service"
                               " && pacman -R --noconfirm blobtorrent && test ! -e /usr/bin/blobtorrent"
                               " && test ! -e /etc/systemd/user/default.target.wants/blobtorrent.service"], timeout=600)
    print("PASS: deb install, global enable, user start, pairing, share, upgrade, remove; rpm and pacman install and remove")
finally:
    for name in ("daemon.log", "launcher.log"):
        file = state / name
        if file.exists():
            (logs / f"package-{name}").write_bytes(file.read_bytes())
    subprocess.run(["journalctl", "--user", "-u", "blobtorrent.service", "--no-pager"],
                   stdout=(logs / "package-journal.log").open("w"), stderr=subprocess.STDOUT)
