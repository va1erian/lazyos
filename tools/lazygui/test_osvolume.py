#!/usr/bin/env python3
"""OS volume sizing for the launcher (`osvolume.py`): a selection that needs
more room than an existing `target/lazyos.img` has is caught before the build
(the build itself cannot resize a volume).

Run: python tools/lazygui/test_osvolume.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog, osvolume  # noqa: E402
from lazygui.testplan import demo_config  # noqa: E402

MIB = osvolume.MIB


def make_image(directory: Path, os_mib: int) -> Path:
    """A disk image whose MBR has an OS partition of ``os_mib`` MiB."""
    mbr = bytearray(512)
    entry = 0x1BE + 2 * 16
    mbr[entry + 4] = 0x83
    mbr[entry + 8:entry + 12] = osvolume.OS_START_LBA.to_bytes(4, "little")
    mbr[entry + 12:entry + 16] = (os_mib * MIB // 512).to_bytes(4, "little")
    mbr[510:512] = b"\x55\xaa"
    path = directory / "lazyos.img"
    path.write_bytes(bytes(mbr))
    return path


class OsVolumeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.tmp = Path(self.dir.name)
        env = {k: v for k, v in osvolume.os.environ.items() if k != "LAZYOS_OS_SIZE"}
        patcher = mock.patch.dict(osvolume.os.environ, env, clear=True)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_the_size_is_read_from_the_mbr(self) -> None:
        self.assertEqual(osvolume.existing_bytes(make_image(self.tmp, 512)), 512 * MIB)
        self.assertIsNone(osvolume.existing_bytes(self.tmp / "missing.img"))

    def test_a_new_image_gets_what_the_selection_needs(self) -> None:
        missing = self.tmp / "none.img"
        self.assertEqual(osvolume.plan(missing, False), (None, False))
        self.assertEqual(osvolume.plan(missing, True), (None, False), "the build default fits")

    def test_a_big_enough_volume_keeps_its_size(self) -> None:
        image = make_image(self.tmp, 1024)
        self.assertEqual(osvolume.plan(image, False), (1024 * MIB, False))
        self.assertEqual(osvolume.plan(image, True), (None, False))
        self.assertEqual(osvolume.env({}, image), {"LAZYOS_OS_SIZE": str(1024 * MIB)})

    def test_a_small_volume_must_be_recreated(self) -> None:
        image = make_image(self.tmp, 512)
        self.assertEqual(osvolume.plan(image, True), (1024 * MIB, True))
        self.assertEqual(osvolume.too_small(image, True), (512, 1024))
        self.assertIsNone(osvolume.too_small(image, False))

    def test_webkit_needs_one_gib(self) -> None:
        self.assertEqual(osvolume.required_bytes(False, True), 1024 * MIB)
        self.assertEqual(osvolume.required_bytes(True, True), 1536 * MIB, "the features add up")
        self.assertEqual(osvolume.required_bytes(True, False), 1024 * MIB)
        self.assertEqual(osvolume.required_bytes(False, False), 512 * MIB)
        missing = self.tmp / "none.img"
        self.assertEqual(osvolume.plan(missing, False, True), (None, False), "the build default fits")
        image = make_image(self.tmp, 512)
        self.assertEqual(osvolume.plan(image, False, True), (1024 * MIB, True))
        self.assertEqual(osvolume.too_small(image, False, True), (512, 1024))
        self.assertIsNone(osvolume.too_small(make_image(self.tmp, 1024), False, True))

    def test_the_webkit_selection_resets_a_small_volume(self) -> None:
        image = make_image(self.tmp, 512)
        demo = demo_config(skip_build=False, webkit=True)
        self.assertEqual(osvolume.cfg_too_small(demo, image), (512, 1024))
        self.assertTrue(osvolume.reset_wanted(demo, image))
        session = {**demo, "mode": "Scripted session"}
        with self.assertRaises(ValueError):
            osvolume.check(session, image)
        self.assertIsNone(osvolume.cfg_too_small(demo_config(skip_build=True, webkit=True), image))
        with mock.patch.object(catalog, "IMAGE", str(image)):
            argv = catalog.build_plan(demo)[-1]["argv"]
            self.assertIn("--reset-os", argv)
            base = {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                    "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}
            env = catalog.build_env({**base, "desktop": False, "webkit": True})
            self.assertEqual(env["LAZYOS_OS_SIZE"], str(1024 * MIB))
        big = make_image(self.tmp, 2048)  # the same path, rewritten
        self.assertEqual(osvolume.env({"webkit": True}, big), {"LAZYOS_OS_SIZE": str(2048 * MIB)})

    def test_srb2kart_beside_the_browser_needs_both(self) -> None:
        """A 1 GiB volume filled up with both and the update died halfway."""
        image = make_image(self.tmp, 1024)
        self.assertEqual(osvolume.plan(image, True, True), (1536 * MIB, True))
        self.assertEqual(osvolume.too_small(image, True, True), (1024, 1536))
        demo = demo_config(skip_build=False, srb2kart=True, lazywebplus=True)
        self.assertEqual(osvolume.cfg_too_small(demo, image), (1024, 1536))
        self.assertTrue(osvolume.reset_wanted(demo, image))
        big = make_image(self.tmp, 1536)  # the same path, rewritten
        self.assertIsNone(osvolume.too_small(big, True, True))

    def test_the_demo_resets_and_other_modes_refuse(self) -> None:
        image = make_image(self.tmp, 512)
        demo = demo_config(skip_build=False, srb2kart=True)
        self.assertTrue(osvolume.reset_wanted(demo, image))
        osvolume.check(demo, image)  # the demo recreates it itself, after asking
        session = {**demo, "mode": "Scripted session"}
        with self.assertRaises(ValueError):
            osvolume.check(session, image)
        osvolume.check({**session, "reset_os": True}, image)
        self.assertFalse(osvolume.reset_wanted(demo_config(skip_build=False), image))

    def test_skip_build_and_an_explicit_size_are_left_alone(self) -> None:
        image = make_image(self.tmp, 512)
        self.assertIsNone(osvolume.cfg_too_small(demo_config(skip_build=True, srb2kart=True), image))
        with mock.patch.dict(osvolume.os.environ, {"LAZYOS_OS_SIZE": "2G"}):
            self.assertEqual(osvolume.env({"srb2kart": True}, image), {})
            self.assertIsNone(osvolume.cfg_too_small({"srb2kart": True}, image))

    def test_the_demo_argv_asks_for_the_reset(self) -> None:
        image = make_image(self.tmp, 512)
        with mock.patch.object(catalog, "IMAGE", str(image)):
            argv = catalog.build_plan(demo_config(skip_build=False, srb2kart=True))[-1]["argv"]
            self.assertIn("--reset-os", argv)
            base = {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                    "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}
            env = catalog.build_env({**base, "desktop": False, "srb2kart": True})
            self.assertEqual(env["LAZYOS_OS_SIZE"], str(1024 * MIB))
            argv = catalog.build_plan(demo_config(skip_build=False))[-1]["argv"]
            self.assertNotIn("--reset-os", argv)

    BASE = {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
            "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def button(self, image: Path, **overrides) -> tuple[list[dict], dict[str, str]]:
        """What the "Build image" button runs for a selection, over ``image``."""
        cfg = {**self.BASE, **demo_config(skip_build=False), **overrides}
        with mock.patch.object(catalog, "IMAGE", str(image)):
            return catalog.image_build(cfg)

    def test_the_build_button_recreates_a_volume_too_small_for_it(self) -> None:
        """The button runs `cargo build` itself: it used to fail at its last step,
        `resizing is not supported, set LAZYOS_RESET_OS=1`, after the whole build."""
        image = make_image(self.tmp, 512)
        steps, env = self.button(image, srb2kart=True, lazywebplus=True)
        self.assertEqual(env["LAZYOS_RESET_OS"], "1")
        self.assertEqual(env["LAZYOS_OS_SIZE"], str(1536 * MIB))
        self.assertEqual(steps[-1]["argv"][1], "build")

    def test_the_build_button_leaves_a_volume_that_fits(self) -> None:
        image = make_image(self.tmp, 1536)
        _steps, env = self.button(image, srb2kart=True, lazywebplus=True)
        self.assertNotIn("LAZYOS_RESET_OS", env)
        self.assertNotIn("LAZYOS_OS_SIZE", env, "the build's own default is that size")

    def test_the_build_button_does_not_take_skip_build_for_a_reason_not_to_build(self) -> None:
        image = make_image(self.tmp, 512)
        _steps, env = self.button(image, skip_build=True, lazywebplus=True)
        self.assertEqual(env["LAZYOS_RESET_OS"], "1")

    def test_the_recreate_box_reaches_cargo_in_every_building_mode(self) -> None:
        image = make_image(self.tmp, 1024)
        with mock.patch.object(catalog, "IMAGE", str(image)):
            for mode in osvolume.BUILDING_MODES:
                cfg = {**self.BASE, **demo_config(skip_build=False), "mode": mode, "reset_os": True}
                self.assertEqual(catalog.build_env(cfg)["LAZYOS_RESET_OS"], "1", mode)
                skipped = {**cfg, "skip_build": True}
                self.assertNotIn("LAZYOS_RESET_OS", catalog.build_env(skipped), mode)
            other = {**self.BASE, **demo_config(skip_build=False), "mode": "Linux ABI bench",
                     "reset_os": True}
            self.assertNotIn("LAZYOS_RESET_OS", catalog.build_env(other))
            unticked = {**self.BASE, **demo_config(skip_build=False)}
            self.assertNotIn("LAZYOS_RESET_OS", catalog.build_env(unticked))

    def test_the_building_modes_are_the_ones_skip_build_applies_to(self) -> None:
        self.assertEqual(osvolume.BUILDING_MODES, catalog.SKIP_BUILD_MODES)


if __name__ == "__main__":
    unittest.main()
