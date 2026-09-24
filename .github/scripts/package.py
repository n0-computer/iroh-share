"""Package native daemon, terminal client, and desktop client release binaries."""

import hashlib
import pathlib
import plistlib
import shutil
import sys
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile


def package(target: str) -> pathlib.Path:
    root = pathlib.Path(__file__).resolve().parents[2]
    binaries = root / "target" / target / "release"
    dist = root / "dist"
    dist.mkdir(exist_ok=True)
    name = f"blobtorrent-{target}"
    windows = "windows" in target
    linux = "linux" in target
    suffix = ".exe" if windows else ""
    with tempfile.TemporaryDirectory() as temporary:
        bundle = pathlib.Path(temporary) / name
        bundle.mkdir()
        for binary in ("blobtorrent", "blobtorrent-tui", "blobtorrent-gui"):
            shutil.copy2(binaries / (binary + suffix), bundle / (binary + suffix))
        if windows:
            shutil.copy2(binaries / "blobtorrent-background.exe", bundle / "blobtorrent-background.exe")
        if linux:
            # Per-user installer: binaries, helper, scripts and a plain-text README at the top.
            shutil.copy2(binaries / "blobtorrent-background", bundle / "blobtorrent-background")
            for script in ("install.sh", "uninstall.sh", "README"):
                shutil.copy2(root / "packaging" / "linux" / script, bundle / script)
                (bundle / script).chmod(0o755 if script.endswith(".sh") else 0o644)
            docs = bundle / "docs"
            docs.mkdir()
        else:
            docs = bundle
        shutil.copy2(root / "README.md", docs / "README.md")
        shutil.copy2(root / "blobtorrent-proto" / "UI.md", docs / "UI.md")
        if "apple" in target:
            with (root / "Cargo.toml").open("rb") as manifest:
                version = tomllib.load(manifest)["workspace"]["package"]["version"]
            contents = bundle / "Blobtorrent.app" / "Contents"
            (contents / "MacOS").mkdir(parents=True)
            for binary in ("blobtorrent", "blobtorrent-background", "blobtorrent-gui", "blobtorrent-tui"):
                shutil.copy2(binaries / binary, contents / "MacOS" / binary)
            with (contents / "Info.plist").open("wb") as file:
                plistlib.dump({
                    "CFBundleExecutable": "blobtorrent-gui",
                    "CFBundleIdentifier": "computer.n0.blobtorrent",
                    "CFBundleName": "Blobtorrent",
                    "CFBundlePackageType": "APPL",
                    "CFBundleShortVersionString": version,
                    "CFBundleVersion": version,
                    "NSHighResolutionCapable": True,
                    "LSMinimumSystemVersion": "13.0",
                    "NSDownloadsFolderUsageDescription": "Share files from Downloads and save downloaded content there.",
                    "NSDocumentsFolderUsageDescription": "Share files from Documents and save downloaded content there.",
                    "NSDesktopFolderUsageDescription": "Share files from Desktop and save downloaded content there.",
                }, file)
            subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(bundle / "Blobtorrent.app")], check=True)
        archive = dist / (name + (".zip" if windows else ".tar.gz"))
        if windows:
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as output:
                for path in sorted(bundle.rglob("*")):
                    if path.is_file():
                        output.write(path, path.relative_to(bundle.parent))
        else:
            with tarfile.open(archive, "w:gz") as output:
                output.add(bundle, arcname=name)
        with archive.open("rb") as file:
            checksum = hashlib.file_digest(file, "sha256").hexdigest()
        archive.with_name(archive.name + ".sha256").write_text(f"{checksum}  {archive.name}\n")
    print(archive)
    return archive


if __name__ == "__main__":
    package(sys.argv[1])
