"""Tests for the BusyBox source fetch/extract in `busybox.py` (offline)."""

import io
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import busybox  # noqa: E402


def _tarball(path: Path, members: dict[str, bytes]) -> None:
    with tarfile.open(path, "w:bz2") as tar:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))


class FetchTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        patches = [
            mock.patch.object(busybox, "BUILD_ROOT", root),
            mock.patch.object(busybox, "SOURCE", root / f"busybox-{busybox.VERSION}"),
            # The fixture archive is not the real release, so skip the pin.
            mock.patch.object(busybox, "_sha256", lambda _path: busybox.SHA256),
        ]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        self.root = root
        self.archive = root / f"busybox-{busybox.VERSION}.tar.bz2"

    def test_a_good_archive_is_published_complete(self) -> None:
        top = f"busybox-{busybox.VERSION}"
        _tarball(self.archive, {f"{top}/Makefile": b"all:\n", f"{top}/Config.in": b""})
        self.assertTrue(busybox._fetch())
        self.assertTrue((busybox.SOURCE / "Config.in").is_file())
        self.assertTrue((busybox.SOURCE / busybox.EXTRACTED_MARK).is_file())
        self.assertEqual([p.name for p in self.root.glob(".extract-*")], [])

    def test_a_partial_tree_with_a_makefile_is_replaced(self) -> None:
        # What a run cancelled mid-extract leaves: the Makefile, but not the rest.
        busybox.SOURCE.mkdir()
        (busybox.SOURCE / "Makefile").write_text("all:\n")
        top = f"busybox-{busybox.VERSION}"
        _tarball(self.archive, {f"{top}/Makefile": b"all:\n", f"{top}/Config.in": b""})
        self.assertTrue(busybox._fetch())
        self.assertTrue((busybox.SOURCE / "Config.in").is_file())

    def test_a_complete_tree_is_kept(self) -> None:
        busybox.SOURCE.mkdir()
        (busybox.SOURCE / busybox.EXTRACTED_MARK).write_text("ok\n")
        (busybox.SOURCE / "sentinel").write_text("x")
        self.assertTrue(busybox._fetch())
        self.assertTrue((busybox.SOURCE / "sentinel").is_file())

    def test_an_archive_without_a_makefile_leaves_nothing_behind(self) -> None:
        _tarball(self.archive, {f"busybox-{busybox.VERSION}/Config.in": b""})
        self.assertFalse(busybox._fetch())
        self.assertFalse(busybox.SOURCE.exists())
        self.assertFalse(self.archive.exists())
        self.assertEqual([p.name for p in self.root.glob(".extract-*")], [])


if __name__ == "__main__":
    unittest.main()
