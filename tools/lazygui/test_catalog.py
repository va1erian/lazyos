#!/usr/bin/env python3
"""Tests for the launcher's home/data-volume plan flags and reset (issues #332, #347, #475).

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
from lazygui.testplan import demo_argv, demo_config  # noqa: E402
# The optional-app switches' tests live beside this file; importing them here
# keeps `python tools/lazygui/test_catalog.py` running every launcher test.
from lazygui.test_catalog_apps import (  # noqa: E402,F401
    DoomTests, EmusicTests, LazyWebTests, LinuxAppsTests, ModPlayerTests, TlsTests, TrayDemoTests,
)
from lazygui.test_display import DisplayModeTests  # noqa: E402,F401
from lazygui.test_login import AutologinTests  # noqa: E402,F401
from lazygui.test_assets import AssetDirTests  # noqa: E402,F401
from lazygui.test_drivers import DriverChoiceTests  # noqa: E402,F401


class HomeDiskPlanTests(unittest.TestCase):
    def test_attached_by_default_at_the_standard_path(self) -> None:
        argv = demo_argv()
        self.assertEqual(argv[argv.index("--home-disk") + 1], catalog.HOME_IMAGE)
        self.assertTrue(catalog.HOME_IMAGE.endswith("home.img"))
        self.assertNotIn("--no-home-disk", argv)

    def test_custom_path_is_passed_through(self) -> None:
        argv = demo_argv(home_path="D:/vols/h.img")
        self.assertEqual(argv[argv.index("--home-disk") + 1], "D:/vols/h.img")

    def test_toggle_off_detaches(self) -> None:
        argv = demo_argv(home_disk=False)
        self.assertIn("--no-home-disk", argv)
        self.assertNotIn("--home-disk", argv)

    def test_the_data_disk_is_off_unless_asked_for(self) -> None:
        argv = demo_argv()
        self.assertNotIn("--data-disk", argv)
        self.assertNotIn("--no-data-disk", argv)

    def test_the_data_disk_stays_reachable(self) -> None:
        argv = demo_argv(data_disk=True)
        self.assertEqual(argv[argv.index("--data-disk") + 1], catalog.DATA_IMAGE)
        argv = demo_argv(data_disk=True, data_path="D:/vols/x.img")
        self.assertEqual(argv[argv.index("--data-disk") + 1], "D:/vols/x.img")


class ResetOsPlanTests(unittest.TestCase):
    def test_off_by_default(self) -> None:
        self.assertNotIn("--reset-os", demo_argv(skip_build=False))

    def test_passed_when_building(self) -> None:
        self.assertIn("--reset-os", demo_argv(skip_build=False, reset_os=True))

    def test_the_gui_confirms_so_the_plan_passes_yes(self) -> None:
        argv = demo_argv(skip_build=False, reset_os=True)
        self.assertIn("--yes", argv)
        self.assertNotIn("--yes", demo_argv(skip_build=False))

    def test_confirmation_asks_only_when_the_reset_will_happen(self) -> None:
        with mock.patch.object(datavol.messagebox, "askyesno", return_value=False) as ask:
            self.assertFalse(datavol.confirm_reset_os(demo_config(skip_build=False, reset_os=True)))
            ask.assert_called_once()
            ask.reset_mock()
            for cfg in (demo_config(skip_build=False), demo_config(reset_os=True),
                        demo_config(mode="Screenshots", skip_build=False, reset_os=True)):
                self.assertTrue(datavol.confirm_reset_os(cfg))
            ask.assert_not_called()
        with mock.patch.object(datavol.messagebox, "askyesno", return_value=True):
            self.assertTrue(datavol.confirm_reset_os(demo_config(skip_build=False, reset_os=True)))

    def test_never_combined_with_skip_build(self) -> None:
        self.assertNotIn("--reset-os", demo_argv(skip_build=True, reset_os=True))


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


class ShellSwitchTests(unittest.TestCase):
    """LazyShell (issue #157): on with the desktop profile, opt out with LAZYOS_SHELL=0."""

    def env(self, **overrides) -> dict[str, str]:
        cfg = {"desktop": True, "services": False, "xuid": False, "xui_client": False,
               "xui_app": "(none)", "shellprobe": False, "msgctl": False, "msgrd": False,
               "busybox": ""}
        cfg.update(overrides)
        return catalog.build_env(cfg)

    def test_desktop_keeps_the_shell_by_default(self) -> None:
        self.assertNotIn("LAZYOS_SHELL", self.env())
        self.assertNotIn("LAZYOS_SHELL", self.env(shell=True))

    def test_unchecking_opts_out(self) -> None:
        self.assertEqual(self.env(shell=False)["LAZYOS_SHELL"], "0")

    def test_non_desktop_images_never_set_it(self) -> None:
        self.assertNotIn("LAZYOS_SHELL", self.env(desktop=False, services=True, xuid=True,
                                                   shell=False))

    def test_simple_desktop_toggle(self) -> None:
        on = catalog.simple_config(demo_config(), "dev", "Desktop")
        self.assertTrue(on["shell"])
        self.assertNotIn("--no-shell", catalog.build_plan(on)[-1]["argv"])
        off = catalog.simple_config(demo_config(skip_build=False), "dev", "Desktop", shell=False)
        self.assertEqual(catalog.build_env(off).get("LAZYOS_SHELL"), "0")
        self.assertIn("--no-shell", catalog.build_plan(off)[-1]["argv"])
        cli = catalog.simple_config(demo_config(), "dev", "CLI", shell=True)
        self.assertFalse(cli["shell"])

    def test_desktop_scripts_that_wait_for_the_terminal_open_it(self) -> None:
        # Nothing opens at boot by default: a desktop session whose script
        # waits for the Terminal must autostart it (or the app it drives).
        examples = os.path.join(catalog.ROOT, "tools", "screenshot", "examples")
        for file, _, switches, stem in catalog.SCRIPTS:
            if "desktop" not in switches:
                continue
            with open(os.path.join(examples, file), encoding="utf-8") as script:
                waits_for_terminal = "TERM:UP:PASS" in script.read()
            if waits_for_terminal:
                self.assertEqual(stem, "term", file)

    def test_shell_demo_is_a_desktop_session(self) -> None:
        entry = [s for s in catalog.SCRIPTS if s[0] == "shell_demo.json"]
        self.assertEqual(len(entry), 1)
        self.assertEqual(entry[0][2], ("desktop",))
        script = os.path.join(catalog.ROOT, "tools", "screenshot", "examples", "shell_demo.json")
        self.assertTrue(os.path.isfile(script))


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

    def test_lazywriter_is_a_document_app_session(self) -> None:
        # Issue #533: LazyWriter ships in every desktop image (no flag of its
        # own); its session autostarts it by its short name.
        self.assertIn("writer", catalog.DOCUMENT_APPS)
        entry = [s for s in catalog.SCRIPTS if s[0] == "xui_writer.json"]
        self.assertEqual(entry, [("xui_writer.json", "XUI app: LazyWriter (format, save, export)",
                                  ("desktop",), "writer")])
        env = self.env("writer")
        self.assertEqual(env["LAZYOS_DESKTOP"], "1")
        self.assertEqual(env["LAZYOS_XUI_AUTOSTART"], "writer")
        self.assertNotIn("LAZYOS_XUI_APPS", env)

    def test_lazywriter_viewer_is_its_built_binary(self) -> None:
        self.assertIn("writer", catalog.XUI_VIEWERS)
        cfg = {"desktop": False, "services": True, "xuid": True, "xui_client": False,
               "xui_app": "writer", "shellprobe": False, "msgctl": False, "msgrd": False,
               "busybox": ""}
        self.assertTrue(catalog.build_env(cfg)["LAZYOS_XUI_APP"].endswith("xui-writer.elf"))

    def test_archiver_is_a_document_app_session(self) -> None:
        # docs/archiver-plan.md: the Archiver ships in every desktop image (no
        # flag of its own); its session autostarts it by its short name.
        self.assertIn("archiver", catalog.DOCUMENT_APPS)
        entry = [s for s in catalog.SCRIPTS if s[0] == "xui_archiver.json"]
        self.assertEqual(len(entry), 1)
        self.assertEqual(entry[0][2:], (("desktop",), "archiver"))
        env = self.env("archiver")
        self.assertEqual(env["LAZYOS_DESKTOP"], "1")
        self.assertEqual(env["LAZYOS_XUI_AUTOSTART"], "archiver")
        self.assertNotIn("LAZYOS_XUI_APPS", env)

    def test_archiver_viewer_is_its_built_binary(self) -> None:
        self.assertIn("archiver", catalog.XUI_VIEWERS)
        cfg = {"desktop": False, "services": True, "xuid": True, "xui_client": False,
               "xui_app": "archiver", "shellprobe": False, "msgctl": False, "msgrd": False,
               "busybox": ""}
        self.assertTrue(catalog.build_env(cfg)["LAZYOS_XUI_APP"].endswith("xui-archiver.elf"))

class LazyRadTests(unittest.TestCase):
    """The launcher can put the LazyRAD IDE on the image (Settings -> Menu offers it)."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "lazyrad": True})
        self.assertEqual(env["LAZYOS_LAZYRAD"], "1")
        # The LazyOS-only samples (Messenger, the MOD player) always come along.
        self.assertEqual(env["LAZYRAD_SAMPLES"].split(os.pathsep),
                         ["lazyrad-os/samples/messenger", "lazyrad-os/samples/modplayer"])

    def test_samples_are_passed_only_with_the_switch(self) -> None:
        on = catalog.build_env({**self.base(), "desktop": True, "lazyrad": True,
                                "lazyrad_samples": os.pathsep.join(["a", "b"])})
        self.assertEqual(on["LAZYRAD_SAMPLES"].split(os.pathsep),
                         ["a", "b", "lazyrad-os/samples/messenger",
                          "lazyrad-os/samples/modplayer"])
        listed = catalog.lazyrad_samples("lazyrad-os/samples/messenger")
        self.assertEqual(listed.split(os.pathsep),
                         ["lazyrad-os/samples/messenger", "lazyrad-os/samples/modplayer"],
                         "no duplicate entry")
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
               "accel": "auto", "memory": "1G", "qemu": "", "out": "shots",
               "times": "10", "lazyrad": True}
        labels = [step["label"] for step in catalog.build_plan(cfg)]
        self.assertEqual(labels[:2], ["Build LazyRAD (static musl)", "Build image (cargo build)"])

    def test_the_build_mode_builds_it_too(self) -> None:
        plan = catalog.build_plan({"mode": "Build xui app", "lazyrad": True})
        self.assertEqual(plan[-1]["argv"][1:], ["tools/lazyrad/build.py"])


class CorePackageTests(unittest.TestCase):
    """Issue #509: the desktop apps are core packages, built after the apps."""

    def labels(self, cfg: dict) -> list[str]:
        return [step["label"] for step in catalog.build_plan(cfg)]

    def test_the_build_mode_packages_after_building(self) -> None:
        plan = catalog.build_plan({"mode": "Build xui app", "lazyrad": False})
        self.assertEqual([s["label"] for s in plan],
                         ["Build xui apps (static musl)", "Build core packages"])
        self.assertEqual(plan[0]["argv"][1:], ["tools/xui/build.py", "--no-core-packages"])
        self.assertEqual(plan[1]["argv"][1:], ["tools/xui/core_packages.py"])

    def test_a_desktop_demo_packages_before_running(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop")
        labels = self.labels(cfg)
        self.assertEqual(labels[:2], ["Build xui apps (static musl)", "Build core packages"])
        self.assertEqual(labels[-1], "Interactive demo")
        self.assertNotIn("Build core packages",
                         self.labels(catalog.simple_config(demo_config(), "dev", "CLI")))

    def test_desktop_scripts_package_the_apps_before_the_image(self) -> None:
        index = {entry[0]: i for i, entry in enumerate(catalog.SCRIPTS)}
        cfg = {"mode": "Scripted session", "script": index["xui_settings.json"], "profile": "dev",
               "skip_build": False, "accel": "auto", "memory": "512M", "qemu": "",
               "out": "shots", "timeout": "300", "tablet": False, "lazyrad": False}
        # Settings' sessions open the Terminal (`term`), so they build the xui
        # apps like the document-app sessions do.
        for script in ("xui_settings.json", "xui_editor.json"):
            self.assertEqual(self.labels({**cfg, "script": index[script]})[:3],
                             ["Build xui apps (static musl)", "Build core packages",
                              "Build image (cargo build)"], script)


class DevicesAppTests(unittest.TestCase):
    """The Devices app (issue #481): shipped with every desktop, opened at
    boot on request, from the Simple tab, the Advanced tab and run_demo."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_opens_only_it_at_boot(self) -> None:
        # The desktop opens nothing at boot by default, so the Terminal is not
        # added with it.
        env = catalog.build_env({**self.base(), "desktop": True, "devices": True})
        self.assertEqual(env["LAZYOS_XUI_AUTOSTART"], catalog.DEVICES_AUTOSTART)
        self.assertEqual(catalog.DEVICES_AUTOSTART, "devices")

    def test_it_joins_a_session_autostart_once(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "devices": True,
                                 "xui_autostart": "editor"})
        self.assertEqual(env["LAZYOS_XUI_AUTOSTART"], "editor,devices")
        env = catalog.build_env({**self.base(), "desktop": True, "devices": True,
                                 "xui_autostart": "devices"})
        self.assertEqual(env["LAZYOS_XUI_AUTOSTART"], "devices")

    def test_off_by_default_and_only_on_a_desktop(self) -> None:
        self.assertNotIn("LAZYOS_XUI_AUTOSTART", catalog.build_env({**self.base(), "desktop": True}))
        self.assertNotIn("LAZYOS_XUI_AUTOSTART",
                         catalog.build_env({**self.base(), "desktop": False, "devices": True}))

    def test_simple_desktop_can_open_it_and_cli_cannot(self) -> None:
        desktop = catalog.simple_config(demo_config(), "dev", "Desktop", devices=True)
        self.assertTrue(desktop["devices"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI", devices=True)["devices"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["devices"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--devices", demo_argv(devices=True, desktop=True, skip_build=False))
        self.assertNotIn("--devices", demo_argv(desktop=True, skip_build=False))
        self.assertNotIn("--devices", demo_argv(devices=True, desktop=True, skip_build=True))
        plan = catalog.build_plan(catalog.simple_config(demo_config(), "dev", "Desktop",
                                                        devices=True))
        self.assertIn("--devices", plan[-1]["argv"])

    def test_it_is_an_xui_viewer_too(self) -> None:
        self.assertIn("devices", catalog.XUI_VIEWERS)
        env = catalog.build_env({**self.base(), "desktop": False, "xuid": True,
                                 "xui_app": "devices"})
        self.assertTrue(env["LAZYOS_XUI_APP"].endswith("xui-devices.elf"))

    def test_run_demo_uses_the_same_autostart(self) -> None:
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
        import run_demo  # noqa: E402
        self.assertEqual(run_demo.DEVICES_AUTOSTART, catalog.DEVICES_AUTOSTART)

    def test_run_demo_keeps_an_existing_autostart_list(self) -> None:
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
        import run_demo  # noqa: E402
        self.assertEqual(run_demo.with_devices(None), "devices")
        self.assertEqual(run_demo.with_devices(""), "devices")
        self.assertEqual(run_demo.with_devices("term"), "term,devices")
        self.assertEqual(run_demo.with_devices("editor"), "editor,devices")
        self.assertEqual(run_demo.with_devices("editor,devices"), "editor,devices")
        self.assertEqual(run_demo.with_devices("term, devices"), "term, devices")


class NetworkTests(unittest.TestCase):
    """Networking from the Simple tab, the Advanced tab and every boot mode:
    the stack in the image, a QEMU card with the forwards (`run_demo --net`)."""

    base = DoomTests.base

    def test_the_switch_builds_the_stack_without_the_harness_clients(self) -> None:
        env = catalog.build_env({**self.base(), "net": True})
        self.assertEqual((env["LAZYOS_NETD"], env["LAZYOS_NETD_ARGS"]), ("1", "demo=0"))
        self.assertNotIn("LAZYOS_NETD", catalog.build_env(self.base()))

    def test_the_demo_passes_run_demos_flags(self) -> None:
        argv = demo_argv(net=True, skip_build=False)
        self.assertIn("--net", argv)
        self.assertNotIn("--net-forward", argv, "empty forwards keep run_demo's default")
        argv = demo_argv(net=True, net_forwards="2323:2323, udp:5353:53", net_restrict=True)
        self.assertEqual(argv[argv.index("--net-forward") + 1], "2323:2323")
        self.assertIn("udp:5353:53", argv)
        self.assertIn("--net-restrict", argv)
        self.assertNotIn("--net", demo_argv(net_forwards="1:2"))

    def test_a_bad_forward_is_a_plan_error(self) -> None:
        with self.assertRaises(ValueError):
            demo_argv(net=True, net_forwards="8080")

    def test_screenshot_and_session_modes_attach_the_card_too(self) -> None:
        common = {"profile": "dev", "skip_build": True, "accel": "auto", "memory": "256M",
                  "qemu": "", "out": "shots", "net": True}
        shot = catalog.build_plan({**common, "mode": "Headless screenshots", "times": "5"})
        self.assertIn("--net", shot[-1]["argv"])
        session = catalog.build_plan({**common, "mode": "Scripted session", "timeout": "60",
                                      "tablet": False, "script": 0})
        self.assertIn("--net", session[-1]["argv"])

    def test_simple_start_offers_it_for_both_interfaces(self) -> None:
        for interface in ("CLI", "Desktop"):
            cfg = catalog.simple_config(demo_config(net_forwards="x", net_restrict=True), "dev",
                                        interface, net=True)
            self.assertTrue(cfg["net"])
            # Stale Advanced forwards never leak into a Simple boot.
            self.assertEqual((cfg["net_forwards"], cfg["net_restrict"]), ("", False))
        self.assertFalse(catalog.simple_config(demo_config(net=True), "dev", "CLI")["net"])


class ResetTests(unittest.TestCase):
    """The Reset button regenerates the home volume layout, and says so first."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = Path(tmp.name) / "home.img"

    def test_summary_names_the_home_directories(self) -> None:
        text = datavol.seed_summary()
        self.assertIn("/admin (700)", text)
        self.assertIn("/user (700)", text)
        self.assertIn("lazyhome", text)
        self.assertNotIn("/tmp", text)

    def test_reset_writes_the_home_layout(self) -> None:
        outcome = datavol.reset(str(self.path), busy=False)  # no file yet: no prompt
        self.assertTrue(outcome and outcome[0])
        image = self.path.read_bytes()
        self.assertIn(b"admin", image)
        self.assertEqual(image[1024 + 120:1024 + 128], b"lazyhome")

    def test_confirmation_lists_what_will_be_created_and_can_decline(self) -> None:
        self.path.write_bytes(b"precious")
        with mock.patch.object(datavol.messagebox, "askyesno", return_value=False) as ask:
            self.assertIsNone(datavol.reset(str(self.path), busy=False))
        self.assertIn("/user (mode 0700, uid 1000, gid 1000)", ask.call_args.args[1])
        self.assertEqual(self.path.read_bytes(), b"precious")

    def test_refused_while_a_run_is_active(self) -> None:
        self.path.write_bytes(b"precious")
        succeeded, _ = datavol.reset(str(self.path), busy=True)
        self.assertFalse(succeeded)
        self.assertEqual(self.path.read_bytes(), b"precious")


class UsbImageTests(unittest.TestCase):
    """The launcher can also write the USB stick image (docs/usb-stick.md)."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_build_variable(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "usb_image": True})
        self.assertEqual(env["LAZYOS_USB_IMAGE"], "1")
        self.assertEqual(env["LAZYOS_USB"], "1")
        self.assertEqual(env["LAZYOS_SERVICES"], "1")

    def test_off_by_default(self) -> None:
        self.assertNotIn("LAZYOS_USB_IMAGE", catalog.build_env({**self.base(), "desktop": True}))


if __name__ == "__main__":
    unittest.main()
