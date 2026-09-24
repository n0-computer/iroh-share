"""Build a macOS package restricted to the current user's home directory."""
import hashlib
import pathlib
import plistlib
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import xml.etree.ElementTree as ET

root = pathlib.Path(__file__).resolve().parents[2]
version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
identifier = "computer.n0.blobtorrent.user"
dist = root / "dist"
with tempfile.TemporaryDirectory(prefix="blobtorrent-pkg-") as temporary:
    work = pathlib.Path(temporary)
    with tarfile.open(dist / "blobtorrent-aarch64-apple-darwin.tar.gz") as archive:
        archive.extractall(work / "archive", filter="data")
    payload = work / "payload"
    applications = payload / "Applications"
    applications.mkdir(parents=True)
    shutil.copytree(work / "archive/blobtorrent-aarch64-apple-darwin/Blobtorrent.app", applications / "Blobtorrent.app")
    uninstaller = applications / "Uninstall Blobtorrent.command"
    shutil.copy2(root / "packaging/macos/Uninstall Blobtorrent.command", uninstaller)
    uninstaller.chmod(0o755)
    scripts = work / "scripts"
    shutil.copytree(root / "packaging/macos/scripts", scripts)
    for script in scripts.iterdir():
        script.chmod(0o755)
    components = work / "components.plist"
    subprocess.run(["pkgbuild", "--analyze", "--root", str(payload), str(components)], check=True)
    with components.open("rb") as file:
        settings = plistlib.load(file)
    for component in settings:
        component["BundleIsRelocatable"] = False
        component["BundleOverwriteAction"] = "upgrade"
    with components.open("wb") as file:
        plistlib.dump(settings, file)
    component_pkg = work / "Blobtorrent-component.pkg"
    subprocess.run(["pkgbuild", "--root", str(payload), "--component-plist", str(components),
                    "--scripts", str(scripts), "--identifier", identifier, "--version", version,
                    "--install-location", "/", str(component_pkg)], check=True)
    distribution = ET.Element("installer-gui-script", {"minSpecVersion": "2"})
    ET.SubElement(distribution, "title").text = "Blobtorrent"
    ET.SubElement(distribution, "welcome", {"file": "welcome.html"})
    ET.SubElement(distribution, "conclusion", {"file": "conclusion.html"})
    ET.SubElement(distribution, "options", {"customize": "never", "require-scripts": "true", "hostArchitectures": "arm64"})
    ET.SubElement(distribution, "domains", {"enable_anywhere": "false", "enable_localSystem": "false", "enable_currentUserHome": "true"})
    allowed = ET.SubElement(distribution, "allowed-os-versions")
    ET.SubElement(allowed, "os-version", {"min": "13.0"})
    outline = ET.SubElement(distribution, "choices-outline")
    ET.SubElement(outline, "line", {"choice": "default"})
    choice = ET.SubElement(distribution, "choice", {"id": "default", "visible": "false"})
    ET.SubElement(choice, "pkg-ref", {"id": identifier})
    ET.SubElement(distribution, "pkg-ref", {"id": identifier, "version": version, "auth": "none"}).text = component_pkg.name
    xml = work / "Distribution.xml"
    ET.ElementTree(distribution).write(xml, encoding="utf-8", xml_declaration=True)
    package = dist / f"blobtorrent-{version}-macos-arm64.pkg"
    subprocess.run(["productbuild", "--distribution", str(xml), "--package-path", str(work),
                    "--resources", str(root / "packaging/macos/resources"), str(package)], check=True)
    with package.open("rb") as file:
        checksum = hashlib.file_digest(file, "sha256").hexdigest()
    package.with_name(package.name + ".sha256").write_text(f"{checksum}  {package.name}\n")
    print(package)
