"""Build deb, rpm and Arch Linux packages from the release binaries with nfpm."""
import hashlib
import os
import pathlib
import subprocess
import sys
import tomllib

ARCHITECTURES = {"x86_64-unknown-linux-gnu": "amd64", "aarch64-unknown-linux-gnu": "arm64"}


def build(target: str) -> list[pathlib.Path]:
    root = pathlib.Path(__file__).resolve().parents[2]
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    dist = root / "dist"
    dist.mkdir(exist_ok=True)
    environment = os.environ | {"ARCH": ARCHITECTURES[target], "VERSION": version}
    # nfpm expands environment variables in metadata but not in content paths.
    template = (root / "packaging" / "linux" / "nfpm.yaml").read_text()
    config = dist / f"nfpm-{target}.yaml"
    config.write_text(template.replace("${BINARIES}", str(root / "target" / target / "release")))
    packages = []
    for packager in ("deb", "rpm", "archlinux"):
        before = set(dist.iterdir())
        subprocess.run(["nfpm", "package", "--config", str(config), "--packager", packager,
                        "--target", str(dist)], cwd=root, env=environment, check=True)
        (package,) = set(dist.iterdir()) - before
        with package.open("rb") as file:
            checksum = hashlib.file_digest(file, "sha256").hexdigest()
        package.with_name(package.name + ".sha256").write_text(f"{checksum}  {package.name}\n")
        print(package)
        packages.append(package)
    config.unlink()
    return packages


if __name__ == "__main__":
    build(sys.argv[1])
