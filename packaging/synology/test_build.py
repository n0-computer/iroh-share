import io
import pathlib
import tarfile
import tempfile
import unittest

import build


def fake_release(dist: pathlib.Path, target: str) -> None:
    with tarfile.open(dist / f"iroh-share-{target}.tar.gz", "w:gz") as archive:
        for binary in build.BINARIES:
            build.add(archive, f"iroh-share-{target}/{binary}", f"{target}/{binary}".encode(), 0o755)


class SpkLayout(unittest.TestCase):
    def test_dsm6_and_dsm7_packages(self):
        with tempfile.TemporaryDirectory() as temporary:
            dist = pathlib.Path(temporary)
            for target in build.ARCHES.values():
                fake_release(dist, target)
            for dsm in (6, 7):
                spk = build.build(dist, dsm)
                self.assertTrue(spk.with_name(spk.name + ".sha256").is_file())
                with tarfile.open(spk) as out:
                    info = dict(
                        line.split("=", 1)
                        for line in out.extractfile("INFO").read().decode().splitlines()
                    )
                    self.assertEqual(info["arch"], '"noarch"')
                    self.assertTrue(info["os_min_ver"].startswith(f'"{dsm}.'))
                    for script in ("start-stop-status", "preupgrade", "postupgrade", *build.NO_OP_SCRIPTS):
                        self.assertTrue(out.getmember(f"scripts/{script}").mode & 0o111, script)
                    self.assertEqual("conf/privilege" in out.getnames(), dsm == 7)
                    self.assertIn("PACKAGE_ICON.PNG", out.getnames())
                    payload = io.BytesIO(out.extractfile("package.tgz").read())
                with tarfile.open(fileobj=payload) as package:
                    self.assertTrue(package.getmember("bin/iroh-share").mode & 0o111)
                    for arch, target in build.ARCHES.items():
                        for binary in build.BINARIES:
                            member = f"bin/{arch}/{binary}"
                            self.assertEqual(package.extractfile(member).read(), f"{target}/{binary}".encode())
                            self.assertTrue(package.getmember(member).mode & 0o111)


if __name__ == "__main__":
    unittest.main()
