#!/usr/bin/env python3
"""Tests for run_demo.py's home volume, reset flags and QEMU disk order (no QEMU, no build).

Run: python tools/test_run_demo.py
"""

from __future__ import annotations

import io
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_demo  # noqa: E402

LABEL_OFFSET = 1024 + 120  # ext2 s_volume_name


class PrepareHomeDiskTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = Path(tmp.name) / "home.img"

    def test_created_when_missing_with_the_home_layout(self) -> None:
        with redirect_stdout(io.StringIO()):
            self.assertTrue(run_demo.prepare_home_disk(self.path, False, False))
        image = self.path.read_bytes()
        self.assertEqual(image[LABEL_OFFSET:LABEL_OFFSET + 8], b"lazyhome")
        self.assertIn(b"alice", image)
        self.assertNotIn(b"tmp\0", image[:1 << 20])

    def test_existing_volume_is_never_regenerated_implicitly(self) -> None:
        self.path.write_bytes(b"precious")
        self.assertTrue(run_demo.prepare_home_disk(self.path, False, False))
        self.assertEqual(self.path.read_bytes(), b"precious")

    def test_existing_volume_does_not_build_the_plan(self) -> None:
        self.path.write_bytes(b"precious")
        plan = mock.Mock(side_effect=AssertionError("planned for nothing"))
        self.assertTrue(run_demo.prepare_volume("home disk", self.path, False, False, plan,
                                                "lazyhome"))
        plan.assert_not_called()

    def test_reset_asks_and_a_decline_leaves_the_volume(self) -> None:
        self.path.write_bytes(b"precious")
        with mock.patch.object(run_demo, "confirm", return_value=False) as ask, \
                redirect_stderr(io.StringIO()):
            self.assertFalse(run_demo.prepare_home_disk(self.path, True, False))
        self.assertIn("/alice (mode 0700", ask.call_args.args[0])
        self.assertEqual(self.path.read_bytes(), b"precious")

    def test_reset_with_yes_skips_the_question(self) -> None:
        self.path.write_bytes(b"precious")
        with mock.patch.object(run_demo, "confirm") as ask, redirect_stdout(io.StringIO()):
            self.assertTrue(run_demo.prepare_home_disk(self.path, True, True))
        ask.assert_not_called()
        self.assertEqual(self.path.read_bytes()[LABEL_OFFSET:LABEL_OFFSET + 8], b"lazyhome")


class MainTests(unittest.TestCase):
    """`main` with the build, QEMU and the filesystem faked out."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.image = self.dir / "lazyos.img"
        self.image.write_bytes(b"\0" * 512)
        self.home = self.dir / "home.img"
        self.builds: list[dict] = []
        self.commands: list[list[str]] = []

    def run_main(self, *argv: str) -> tuple[int, list[str]]:
        launched: list[str] = []

        def fake_build(command, cwd=None, env=None, **_):
            self.commands.append([str(part) for part in command])
            if env is not None:
                self.builds.append(env)
            return mock.Mock(returncode=0)

        with mock.patch.object(run_demo.busybox, "ensure_busybox"), \
                mock.patch.object(run_demo, "build_rhai"), \
                mock.patch.object(run_demo, "find_qemu", return_value="qemu"), \
                mock.patch.object(run_demo, "accel_args", return_value=[]), \
                mock.patch.object(run_demo.subprocess, "run", fake_build), \
                mock.patch.object(run_demo.subprocess, "call",
                                  lambda command: launched.extend(command) or 0), \
                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            code = run_demo.main(["--image", str(self.image), "--home-disk", str(self.home),
                                  *argv])
        return code, launched

    def test_home_disk_is_created_and_attached_second(self) -> None:
        code, command = self.run_main("--no-build")
        self.assertEqual(code, 0)
        self.assertTrue(self.home.is_file())
        self.assertIn("virtio-blk-pci,drive=home", command)
        self.assertLess(command.index("virtio-blk-pci,drive=boot"),
                        command.index("virtio-blk-pci,drive=home"))

    def test_data_disk_is_not_attached_by_default(self) -> None:
        _, command = self.run_main("--no-build")
        self.assertFalse([arg for arg in command if "drive=data" in arg])

    def test_data_disk_still_works_and_precedes_home(self) -> None:
        data = self.dir / "data.img"
        code, command = self.run_main("--no-build", "--data-disk", str(data))
        self.assertEqual(code, 0)
        self.assertTrue(data.is_file())
        self.assertLess(command.index("virtio-blk-pci,drive=data"),
                        command.index("virtio-blk-pci,drive=home"))

    def test_no_home_disk_boots_with_the_boot_disk_only(self) -> None:
        code, command = self.run_main("--no-build", "--no-home-disk")
        self.assertEqual(code, 0)
        self.assertFalse(self.home.exists())
        self.assertEqual(sum("virtio-blk-pci" in arg for arg in command), 1)

    def test_reset_home_without_a_tty_declines(self) -> None:
        self.home.write_bytes(b"precious")
        code, _ = self.run_main("--no-build", "--reset-home")  # stdin is not a TTY under test
        self.assertEqual(code, 1)
        self.assertEqual(self.home.read_bytes(), b"precious")

    def test_reset_home_with_yes_reformats(self) -> None:
        self.home.write_bytes(b"precious")
        code, _ = self.run_main("--no-build", "--reset-home", "--yes")
        self.assertEqual(code, 0)
        self.assertEqual(self.home.read_bytes()[LABEL_OFFSET:LABEL_OFFSET + 8], b"lazyhome")

    def test_reset_os_sets_the_build_variable(self) -> None:
        code, _ = self.run_main("--reset-os", "--yes")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1].get("LAZYOS_RESET_OS"), "1")

    def test_reset_os_without_a_tty_declines_before_building(self) -> None:
        code, _ = self.run_main("--reset-os")  # stdin is not a TTY under test
        self.assertEqual(code, 1)
        self.assertEqual(self.builds, [])

    def test_reset_os_asks_and_a_no_stops_the_build(self) -> None:
        with mock.patch.object(run_demo, "confirm", return_value=False) as ask:
            code, _ = self.run_main("--reset-os")
        self.assertEqual(code, 1)
        self.assertEqual(self.builds, [])
        self.assertIn("OS volume", ask.call_args.args[0])

    def test_reset_os_with_a_yes_answer_builds(self) -> None:
        with mock.patch.object(run_demo, "confirm", return_value=True):
            code, _ = self.run_main("--reset-os")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1].get("LAZYOS_RESET_OS"), "1")

    def test_reset_os_with_no_existing_image_has_nothing_to_confirm(self) -> None:
        self.image.unlink()
        with mock.patch.object(run_demo, "confirm") as ask:
            self.run_main("--reset-os")
        ask.assert_not_called()
        self.assertEqual(self.builds[-1].get("LAZYOS_RESET_OS"), "1")

    def test_build_leaves_reset_os_unset_by_default(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("LAZYOS_RESET_OS", None)
            self.run_main()
        self.assertNotIn("LAZYOS_RESET_OS", self.builds[-1])

    def test_a_desktop_build_packages_the_core_apps_first(self) -> None:
        # Issue #509: the desktop apps ship as core packages, so the image
        # build needs `target/pkg/core` from the apps just built.
        with mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, _ = self.run_main("--desktop")
        self.assertEqual(code, 0)
        scripts = [Path(command[1]).name for command in self.commands if len(command) > 1]
        self.assertIn("core_packages.py", scripts)
        packaged = scripts.index("core_packages.py")
        cargo = next(i for i, command in enumerate(self.commands) if "cargo" in command[0])
        self.assertLess(packaged, cargo)
        self.assertEqual(self.builds[-1].get("LAZYOS_DESKTOP"), "1")

    def test_a_console_build_does_not_package_apps(self) -> None:
        code, _ = self.run_main()
        self.assertEqual(code, 0)
        self.assertFalse(any("core_packages.py" in " ".join(c) for c in self.commands))

    def test_reset_os_cannot_combine_with_no_build(self) -> None:
        with self.assertRaises(SystemExit), redirect_stderr(io.StringIO()):
            self.run_main("--no-build", "--reset-os")


if __name__ == "__main__":
    unittest.main()
