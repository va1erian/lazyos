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


class LineEditingTests(unittest.TestCase):
    def test_options_replace_disabled_and_differing_lines_and_fill_gaps(self) -> None:
        lines = [
            "CONFIG_STATIC=y",
            "# CONFIG_FEATURE_TAB_COMPLETION is not set",
            "CONFIG_FEATURE_EDITING_HISTORY=16",
        ]
        out = busybox.apply_line_editing(lines)
        self.assertIn("CONFIG_STATIC=y", out)
        self.assertNotIn("# CONFIG_FEATURE_TAB_COMPLETION is not set", out)
        self.assertNotIn("CONFIG_FEATURE_EDITING_HISTORY=16", out)
        for key, value in busybox.LINE_EDITING.items():
            self.assertEqual(out.count(f"CONFIG_{key}={value}"), 1, key)

    def test_native_configure_applies_line_editing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp)
            (source / ".config").write_text(
                "# CONFIG_STATIC is not set\n# CONFIG_FEATURE_TAB_COMPLETION is not set\n",
                encoding="utf-8",
            )
            with mock.patch.object(busybox, "SOURCE", source), \
                    mock.patch.object(busybox.subprocess, "run") as run:
                self.assertTrue(busybox._configure())
            config = (source / ".config").read_text(encoding="utf-8").splitlines()
            self.assertIn("CONFIG_STATIC=y", config)
            for key, value in busybox.LINE_EDITING.items():
                self.assertIn(f"CONFIG_{key}={value}", config)
            commands = [call.args[0] for call in run.call_args_list]
            self.assertEqual(commands, [["make", "defconfig"], ["make", "oldconfig"]])

    def test_tab_completion_is_requested(self) -> None:
        self.assertEqual(busybox.LINE_EDITING["FEATURE_TAB_COMPLETION"], "y")
        self.assertEqual(busybox.LINE_EDITING["FEATURE_EDITING"], "y")

    def test_docker_script_sets_every_option(self) -> None:
        for key, value in busybox.LINE_EDITING.items():
            self.assertIn(f"echo CONFIG_{key}={value} >> .config", busybox._DOCKER_SCRIPT)

    def test_a_build_with_another_configuration_is_not_reused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "busybox"
            out.write_bytes(b"x")
            stamp = Path(tmp) / busybox.CONFIG_STAMP.name
            with mock.patch.object(busybox, "OUTPUT", out), \
                    mock.patch.object(busybox, "MANUAL", Path(tmp) / "none"), \
                    mock.patch.object(busybox, "_main_checkout_build", lambda: None):
                self.assertIsNone(busybox._cached())  # no stamp: an old build
                stamp.write_text("stale\n")
                self.assertIsNone(busybox._cached())
                stamp.write_text(busybox.config_stamp() + "\n")
                self.assertEqual(busybox._cached(), out)
                self.assertEqual(busybox._stale(), out)


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
