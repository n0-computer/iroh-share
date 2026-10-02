"""Prepare downloaded build artifacts for a release.

Gives the binary archives versioned names like the installers, replaces the
per-file checksums with one SHA256SUMS, and prints the download section that
heads the release notes.

Usage: release.py DIST REPO TAG
"""
import hashlib
from pathlib import Path
import sys

GATEWAY = "https://github.com/n0-computer/iroh-content-discovery/releases/latest"
ARCHIVES = {
    "x86_64-unknown-linux-musl.tar.gz": "linux-x64.tar.gz",
    "aarch64-unknown-linux-musl.tar.gz": "linux-arm64.tar.gz",
    "aarch64-apple-darwin.tar.gz": "macos-arm64.tar.gz",
    "x86_64-pc-windows-msvc.zip": "windows-x64.zip",
}


def prepare(dist, version):
    for checksum in dist.glob("*.sha256"):
        checksum.unlink()
    for target, name in ARCHIVES.items():
        archive = dist / f"iroh-share-{target}"
        if archive.exists():
            archive.rename(dist / f"iroh-share-{version}-{name}")
    files = sorted(path for path in dist.iterdir() if path.is_file())
    lines = [f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n" for path in files]
    (dist / "SHA256SUMS").write_text("".join(lines))
    return sorted(path.name for path in dist.iterdir())


def notes(repo, tag, version, files):
    base = f"https://github.com/{repo}/releases/download/{tag}"

    def link(name, label=None):
        return f"[{label or name}]({base}/{name})" if name in files else None

    def line(title, detail, *links):
        links = [item for item in links if item]
        detail = f" ({detail})" if detail else ""
        return f"- **{title}**{detail}: " + " · ".join(links) if links else None

    name = f"iroh-share-{version}"
    lines = [
        "## Download",
        "",
        line("macOS", "Apple silicon", link(f"{name}-macos-arm64.pkg")),
        line("Windows", "x64", link(f"{name}-windows-x64-setup.exe")),
        line("Synology NAS", None,
             link(f"{name}-dsm7.spk", "DSM 7"), link(f"{name}-dsm6.spk", "DSM 6")),
        line("Linux", "daemon, CLI and TUI",
             link(f"{name}-linux-x64.tar.gz", "x64"), link(f"{name}-linux-arm64.tar.gz", "arm64")),
        line("Archives without installer", None,
             link(f"{name}-macos-arm64.tar.gz", "macOS"), link(f"{name}-windows-x64.zip", "Windows")),
        f"- Checksums: {link('SHA256SUMS')}" if "SHA256SUMS" in files else None,
        "",
        f"To browse blake3.net and pkarr.net links, install [Iroh Link Gateway]({GATEWAY}) "
        "and its browser extension.",
        "",
    ]
    return "\n".join(item for item in lines if item is not None)


if __name__ == "__main__":
    dist, repo, tag = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
    version = tag.removeprefix("v")
    print(notes(repo, tag, version, prepare(dist, version)))
