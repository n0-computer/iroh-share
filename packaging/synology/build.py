"""Build Synology packages (.spk) for DSM 6 and DSM 7.

Takes the static Linux release archives for x86_64 and aarch64 from dist/
and writes iroh-share-<version>-dsm6.spk and -dsm7.spk with checksums.
Both are noarch: bin/iroh-share picks the binary for the NAS at runtime.
"""
import hashlib
import io
import json
import pathlib
import sys
import tarfile
import time
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = ROOT / "packaging" / "synology"
ARCHES = {"x86_64": "x86_64-unknown-linux-musl", "aarch64": "aarch64-unknown-linux-musl"}
BINARIES = ("iroh-share", "iroh-share-tui")
# Package Center expects these scripts; they have nothing to do here.
NO_OP_SCRIPTS = ("preinst", "postinst", "preuninst", "postuninst")


def info(version: str, dsm: int) -> str:
    fields = {
        "package": "iroh-share",
        "version": f"{version}-1",
        "displayname": "Iroh Share",
        "description": "Share and download content-addressed files peer to peer over iroh.",
        "maintainer": "n0-computer",
        "maintainer_url": "https://github.com/n0-computer/iroh-share",
        "arch": "noarch",
        "thirdparty": "yes",
    }
    if dsm == 7:
        fields |= {"os_min_ver": "7.0-40000", "ctl_stop": "yes"}
    else:
        fields |= {"os_min_ver": "6.1-15047", "firmware": "6.1-15047", "startable": "yes"}
    return "".join(f'{key}="{value}"\n' for key, value in fields.items())


def add(archive: tarfile.TarFile, name: str, data: bytes, mode: int) -> None:
    member = tarfile.TarInfo(name)
    member.size = len(data)
    member.mode = mode
    member.mtime = int(time.time())
    archive.addfile(member, io.BytesIO(data))


def add_dir(archive: tarfile.TarFile, name: str) -> None:
    member = tarfile.TarInfo(name)
    member.type = tarfile.DIRTYPE
    member.mode = 0o755
    member.mtime = int(time.time())
    archive.addfile(member)


def payload(dist: pathlib.Path) -> bytes:
    """The files installed to /var/packages/iroh-share/target."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as package:
        add(package, "bin/iroh-share", (HERE / "bin" / "iroh-share").read_bytes(), 0o755)
        for license in ("LICENSE-APACHE", "LICENSE-MIT"):
            add(package, license, (ROOT / license).read_bytes(), 0o644)
        for arch, target in ARCHES.items():
            with tarfile.open(dist / f"iroh-share-{target}.tar.gz") as release:
                for binary in BINARIES:
                    data = release.extractfile(f"iroh-share-{target}/{binary}").read()
                    add(package, f"bin/{arch}/{binary}", data, 0o755)
    return buffer.getvalue()


def build(dist: pathlib.Path, dsm: int) -> pathlib.Path:
    with (ROOT / "Cargo.toml").open("rb") as manifest:
        version = tomllib.load(manifest)["workspace"]["package"]["version"]
    spk = dist / f"iroh-share-{version}-dsm{dsm}.spk"
    with tarfile.open(spk, "w") as out:
        add(out, "INFO", info(version, dsm).encode(), 0o644)
        add(out, "package.tgz", payload(dist), 0o644)
        add(out, "PACKAGE_ICON.PNG", (ROOT / "assets" / "icon-72.png").read_bytes(), 0o644)
        add(out, "PACKAGE_ICON_256.PNG", (ROOT / "assets" / "icon-256.png").read_bytes(), 0o644)
        add_dir(out, "scripts")
        for script in sorted((HERE / "scripts").iterdir()):
            add(out, f"scripts/{script.name}", script.read_bytes(), 0o755)
        for name in NO_OP_SCRIPTS:
            add(out, f"scripts/{name}", b"#!/bin/sh\nexit 0\n", 0o755)
        if dsm == 7:
            # DSM 7 runs third-party packages as their own user.
            add_dir(out, "conf")
            privilege = json.dumps({"defaults": {"run-as": "package"}}).encode()
            add(out, "conf/privilege", privilege, 0o644)
    checksum = hashlib.sha256(spk.read_bytes()).hexdigest()
    spk.with_name(spk.name + ".sha256").write_text(f"{checksum}  {spk.name}\n")
    print(spk)
    return spk


if __name__ == "__main__":
    dist = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "dist"
    for dsm in (6, 7):
        build(dist, dsm)
