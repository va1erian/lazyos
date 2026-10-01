#!/usr/bin/env python3
"""Tests for the launcher's data-volume plan flags and reset (issues #332, #347).

Run: python tools/lazygui/test_catalog.py
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog, datavol  # noqa: E402


def demo_config(**overrides) -> dict:
    """A minimal Interactive-demo configuration."""
    cfg = {"mode": "Interactive demo", "profile": "dev", "skip_build": True,
           "headless": False, "accel": "auto", "memory": "256M", "qemu": "", "extra": ""}
    cfg.update(overrides)
    return cfg


def demo_argv(**overrides) -> list[str]:
    return catalog.build_plan(demo_config(**overrides))[-1]["argv"]


class DataDiskPlanTests(unittest.TestCase):
    def test_attached_by_default_at_the_standard_path(self) -> None:
        argv = demo_argv()
        self.assertEqual(argv[argv.index("--data-disk") + 1], catalog.DATA_IMAGE)
        self.assertNotIn("--no-data-disk", argv)

    def test_custom_path_is_passed_through(self) -> None:
        argv = demo_argv(data_path="D:/vols/x.img")
        self.assertEqual(argv[argv.index("--data-disk") + 1], "D:/vols/x.img")

    def test_toggle_off_detaches(self) -> None:
        argv = demo_argv(data_disk=False)
        self.assertIn("--no-data-disk", argv)
        self.assertNotIn("--data-disk", argv)


class SoundPlanTests(unittest.TestCase):
    """The launcher attaches a sound card for the desktop (`beep` in the Terminal)."""

    def test_desktop_simple_start_enables_sound(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop")
        self.assertTrue(cfg["sound"])
        self.assertIn("--sound", catalog.build_plan(cfg)[-1]["argv"])

    def test_cli_simple_start_stays_quiet(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "CLI")
        self.assertFalse(cfg["sound"])
        self.assertNotIn("--sound", catalog.build_plan(cfg)[-1]["argv"])

    def test_advanced_checkbox_is_honoured(self) -> None:
        self.assertIn("--sound", demo_argv(sound=True))
        self.assertNotIn("--sound", demo_argv(sound=False))
        self.assertNotIn("--sound", demo_argv())


class DocumentAppSessionTests(unittest.TestCase):
    """Editor/Paint/Files scripts boot the desktop profile, autostarting one app."""

    def env(self, stem: str) -> dict[str, str]:
        cfg = {"desktop": True, "services": False, "xuid": False, "xui_client": False,
               "xui_app": "(none)", "shellprobe": False, "msgctl": False, "msgrd": False,
               "busybox": "", "xui_autostart": stem}
        return catalog.build_env(cfg)

    def test_document_scripts_are_desktop_sessions(self) -> None:
        for file, _, switches, stem in catalog.SCRIPTS:
            if stem in catalog.DOCUMENT_APPS:
                self.assertEqual(switches, ("desktop",), file)

    def test_autostart_names_the_app_and_all_apps_are_embedded(self) -> None:
        env = self.env("files")
        self.assertEqual(env["LAZYOS_DESKTOP"], "1")
        self.assertEqual(env["LAZYOS_XUI_AUTOSTART"], "files")
        self.assertNotIn("LAZYOS_XUI_APP", env)
        # The desktop profile embeds its own default app set (build.rs); the
        # GUI must not override it with a list of its own.
        self.assertNotIn("LAZYOS_XUI_APPS", env)

    def test_no_autostart_without_a_document_script(self) -> None:
        self.assertNotIn("LAZYOS_XUI_AUTOSTART", self.env(""))

class LazyRadTests(unittest.TestCase):
    """The launcher can put the LazyRAD IDE on the image (Settings -> Menu offers it)."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "lazyrad": True})
        self.assertEqual(env["LAZYOS_LAZYRAD"], "1")
        self.assertNotIn("LAZYRAD_SAMPLES", env)

    def test_samples_are_passed_only_with_the_switch(self) -> None:
        on = catalog.build_env({**self.base(), "desktop": True, "lazyrad": True,
                                "lazyrad_samples": "C:\a;C:\b"})
        self.assertEqual(on["LAZYRAD_SAMPLES"], "C:\a;C:\b")
        off = catalog.build_env({**self.base(), "desktop": True, "lazyrad": False,
                                 "lazyrad_samples": "C:\a"})
        self.assertNotIn("LAZYOS_LAZYRAD", off)
        self.assertNotIn("LAZYRAD_SAMPLES", off)

    def test_off_by_default(self) -> None:
        self.assertNotIn("LAZYOS_LAZYRAD", catalog.build_env({**self.base(), "desktop": True}))

    def test_simple_desktop_can_include_it_and_cli_cannot(self) -> None:
        desktop = catalog.simple_config(demo_config(), "dev", "Desktop", lazyrad=True)
        self.assertTrue(desktop["lazyrad"])
        cli = catalog.simple_config(demo_config(), "dev", "CLI", lazyrad=True)
        self.assertFalse(cli["lazyrad"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["lazyrad"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        # run_demo.py builds LazyRAD itself, so the GUI and the CLI agree.
        self.assertIn("--lazyrad", demo_argv(lazyrad=True, skip_build=False))
        self.assertNotIn("--lazyrad", demo_argv(skip_build=False))
        # "Skip build" boots the existing image: no LazyRAD build is requested.
        self.assertNotIn("--lazyrad", demo_argv(lazyrad=True, skip_build=True))

    def test_session_modes_build_it_before_the_image(self) -> None:
        cfg = {"mode": "Headless screenshots", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "256M", "qemu": "", "out": "shots",
               "times": "10", "lazyrad": True}
        labels = [step["label"] for step in catalog.build_plan(cfg)]
        self.assertEqual(labels[:2], ["Build LazyRAD (static musl)", "Build image (cargo build)"])

    def test_the_build_mode_builds_it_too(self) -> None:
        plan = catalog.build_plan({"mode": "Build xui app", "lazyrad": True})
        self.assertEqual(plan[-1]["argv"][1:], ["tools/lazyrad/build.py"])


class ResetTests(unittest.TestCase):
    """The Reset button regenerates the seeded layout, and says so first."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = Path(tmp.name) / "data.img"

    def test_summary_names_the_seeded_directories(self) -> None:
        text = datavol.seed_summary()
        self.assertIn("/home/alice", text)
        self.assertIn("/tmp (1777)", text)

    def test_reset_writes_the_seeded_layout(self) -> None:
        outcome = datavol.reset(str(self.path), busy=False)  # no file yet: no prompt
        self.assertTrue(outcome and outcome[0])
        self.assertIn(b"alice", self.path.read_bytes())

    def test_confirmation_lists_what_will_be_created_and_can_decline(self) -> None:
        self.path.write_bytes(b"precious")
        with mock.patch.object(datavol.messagebox, "askyesno", return_value=False) as ask:
            self.assertIsNone(datavol.reset(str(self.path), busy=False))
        self.assertIn("/home/alice (mode 0755, uid 1000, gid 1000)", ask.call_args.args[1])
        self.assertEqual(self.path.read_bytes(), b"precious")

    def test_refused_while_a_run_is_active(self) -> None:
        self.path.write_bytes(b"precious")
        succeeded, _ = datavol.reset(str(self.path), busy=True)
        self.assertFalse(succeeded)
        self.assertEqual(self.path.read_bytes(), b"precious")


if __name__ == "__main__":
    unittest.main()
