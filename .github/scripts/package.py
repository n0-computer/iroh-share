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
    name = f"iroh-share-{target}"
    windows = "windows" in target
    suffix = ".exe" if windows else ""
    with tempfile.TemporaryDirectory() as temporary:
        bundle = pathlib.Path(temporary) / name
        bundle.mkdir()
        for binary in ("iroh-share", "iroh-share-tui", "iroh-share-gui"):
            shutil.copy2(binaries / (binary + suffix), bundle / (binary + suffix))
        if windows:
            shutil.copy2(binaries / "iroh-share-background.exe", bundle / "iroh-share-background.exe")
        shutil.copy2(root / "README.md", bundle / "README.md")
        shutil.copy2(root / "iroh-share-proto" / "UI.md", bundle / "UI.md")
        if "apple" in target:
            with (root / "Cargo.toml").open("rb") as manifest:
                version = tomllib.load(manifest)["workspace"]["package"]["version"]
            contents = bundle / "Iroh Share.app" / "Contents"
            (contents / "MacOS").mkdir(parents=True)
            for binary in ("iroh-share", "iroh-share-background", "iroh-share-gui", "iroh-share-tui"):
                shutil.copy2(binaries / binary, contents / "MacOS" / binary)
            with (contents / "Info.plist").open("wb") as file:
                plistlib.dump({
                    "CFBundleExecutable": "iroh-share-gui",
                    "CFBundleIdentifier": "computer.n0.iroh-share",
                    "CFBundleName": "Iroh Share",
                    "CFBundlePackageType": "APPL",
                    "CFBundleShortVersionString": version,
                    "CFBundleVersion": version,
                    "NSHighResolutionCapable": True,
                    "LSMinimumSystemVersion": "13.0",
                    "NSDownloadsFolderUsageDescription": "Share files from Downloads and save downloaded content there.",
                    "NSDocumentsFolderUsageDescription": "Share files from Documents and save downloaded content there.",
                    "NSDesktopFolderUsageDescription": "Share files from Desktop and save downloaded content there.",
                }, file)
            subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(bundle / "Iroh Share.app")], check=True)
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
