#!/usr/bin/env python3
"""The stick GUI's logic, without a display.

    python tools/boot/test_stick_gui.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import stick_gui  # noqa: E402
import write_stick  # noqa: E402


class StickGuiTest(unittest.TestCase):
    def test_build_step_builds_without_booting(self) -> None:
        argv = stick_gui.build_step(release=True)["argv"]
        self.assertTrue(argv[2].endswith("run_demo.py"))
        for flag in ("--desktop", "--usb-image", "--build-only", "--release"):
            self.assertIn(flag, argv)
        self.assertNotIn("--release", stick_gui.build_step(release=False)["argv"])
        self.assertEqual(stick_gui.build_env("8G"), {"LAZYOS_USB_HOME_SIZE": "8G"})

    def test_write_step_skips_the_prompts_with_absolute_paths(self) -> None:
        argv = stick_gui.write_step(["pkexec"], Path("x.img"), "/dev/sdz")["argv"]
        self.assertEqual(argv[0], "pkexec")
        self.assertTrue(Path(argv[1]).is_absolute())
        self.assertTrue(argv[3].endswith("write_stick.py"))
        self.assertTrue(Path(argv[argv.index("--image") + 1]).is_absolute())
        self.assertEqual(argv[argv.index("--device") + 1], "/dev/sdz")
        self.assertEqual(argv[-1], "--yes")

    def test_progress_reads_the_writer_lines(self) -> None:
        self.assertEqual(stick_gui.progress("[+   1.000s] wrote 12 / 1134 MiB\n"),
                         ("wrote", 12, 1134))
        self.assertEqual(stick_gui.progress("verified 1134 / 1134 MiB"),
                         ("verified", 1134, 1134))
        self.assertIsNone(stick_gui.progress("verified: sha256 abc"))

    def test_disk_rows_carry_the_writer_refusals(self) -> None:
        with tempfile.NamedTemporaryFile() as image:
            image.truncate(4 << 20)
            disks = [write_stick.Disk("/dev/sdy", 8 << 30, "Stick", True, True, []),
                     write_stick.Disk("/dev/sdz", 1 << 20, "Tiny", True, True, []),
                     write_stick.Disk("/dev/sdx", 8 << 30, "Busy", True, True, ["/dev/sdx1"])]
            reasons = [reason for _, reason in stick_gui.disk_rows(disks, Path(image.name))]
        self.assertIsNone(reasons[0])
        self.assertIn("image needs", reasons[1])
        if write_stick.os.name != "nt":
            self.assertIn("unmount", reasons[2])

    def test_image_summary(self) -> None:
        self.assertIn("Build image", stick_gui.image_summary(Path("/nonexistent.img")))
        with tempfile.NamedTemporaryFile() as image:
            image.truncate(3 << 20)
            self.assertTrue(stick_gui.image_summary(Path(image.name)).startswith("3 MiB"))

    def test_writer_yes_skips_confirm(self) -> None:
        asked = []
        disk = write_stick.Disk("/dev/sdq", 8 << 30, "Stick", True, True, [])
        with tempfile.NamedTemporaryFile() as image:
            image.truncate(1 << 20)
            orig = (write_stick.list_disks, write_stick.confirm, write_stick.write_and_verify)
            write_stick.list_disks = lambda: [disk]
            write_stick.confirm = lambda *a: asked.append(a) or False
            write_stick.write_and_verify = lambda *a: None
            try:
                code = write_stick.main(["--image", image.name, "--device", "/dev/sdq", "--yes"])
                self.assertEqual((code, asked), (0, []))
                code = write_stick.main(["--image", image.name, "--device", "/dev/sdq"])
                self.assertEqual((code, len(asked)), (1, 1))
            finally:
                write_stick.list_disks, write_stick.confirm, write_stick.write_and_verify = orig


if __name__ == "__main__":
    unittest.main()
