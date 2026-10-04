#!/usr/bin/env python3
"""Launcher tests for the optional apps an image can embed: Doom, the LazyRAD
MOD player and the Linux programs (from the Simple tab, the Advanced tab and
run_demo). `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_apps.py
"""

from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class DoomTests(unittest.TestCase):
    """The launcher can put the Doom package on the image
    (`/system/share/samples/doom.lzp`, then `pkgctl install`), from the Simple tab, the Advanced tab and run_demo."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "doom": True})
        self.assertEqual(env["LAZYOS_DOOM"], "1")
        self.assertNotIn("LAZYOS_DOOM", catalog.build_env({**self.base(), "desktop": True}))

    def test_simple_desktop_can_include_it_and_cli_cannot(self) -> None:
        self.assertTrue(catalog.simple_config(demo_config(), "dev", "Desktop", doom=True)["doom"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI", doom=True)["doom"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["doom"])

    def test_advanced_sessions_get_the_card_with_tls_alone(self) -> None:
        self.assertEqual(catalog.net_flags({"net": False, "tls": False}), [])
        self.assertIn("--net", catalog.net_flags({"net": False, "tls": True}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--doom", demo_argv(doom=True, skip_build=False))
        self.assertNotIn("--doom", demo_argv(skip_build=False))
        self.assertNotIn("--doom", demo_argv(doom=True, skip_build=True))

    def test_session_modes_build_the_package_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "1G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "doom": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(labels[at - 1], "Build Doom package (engine + Freedoom)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/doom/build.py", "--require"])

    def test_both_optional_apps_build_in_order(self) -> None:
        steps = catalog.app_steps({"lazyrad": True, "doom": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/lazyrad/build.py", "tools/doom/build.py"])
        self.assertEqual(catalog.app_steps({}), [])


class ModPlayerTests(unittest.TestCase):
    """The LazyRAD MOD player as a package (`/system/share/samples/modplayer.lzp`, then `pkgctl
    install`), from the Simple tab, the Advanced tab and run_demo."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_embeds_the_package_and_lazyrad(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "modplayer": True})
        self.assertEqual(env["LAZYOS_MODPLAYER"], "1")
        self.assertEqual(env["LAZYOS_LAZYRAD"], "1", "the package carries LazyRAD's player")
        self.assertIn("lazyrad-os/samples/modplayer", env["LAZYRAD_SAMPLES"].split(os.pathsep))
        self.assertNotIn("LAZYOS_MODPLAYER", catalog.build_env({**self.base(), "desktop": True}))

    def test_simple_desktop_can_include_it_and_cli_cannot(self) -> None:
        on = catalog.simple_config(demo_config(), "dev", "Desktop", modplayer=True)
        self.assertTrue(on["modplayer"])
        self.assertTrue(on["sound"], "a desktop has a sound card")
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI",
                                               modplayer=True)["modplayer"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["modplayer"])

    def test_advanced_sessions_get_the_card_with_tls_alone(self) -> None:
        self.assertEqual(catalog.net_flags({"net": False, "tls": False}), [])
        self.assertIn("--net", catalog.net_flags({"net": False, "tls": True}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--modplayer", demo_argv(modplayer=True, skip_build=False))
        self.assertNotIn("--modplayer", demo_argv(skip_build=False))
        self.assertNotIn("--modplayer", demo_argv(modplayer=True, skip_build=True))

    def test_the_package_is_built_after_the_player_and_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "256M", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "modplayer": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(labels[at - 2:at], ["Build LazyRAD (static musl)",
                                             "Package the MOD player (LazyRAD)"])
        self.assertEqual(plan[at - 1]["argv"][1:],
                         ["tools/lazyrad/package.py", "--no-build", "--require"])

    def test_every_optional_app_builds_in_order(self) -> None:
        steps = catalog.app_steps({"lazyrad": True, "modplayer": True, "doom": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/lazyrad/build.py", "tools/lazyrad/package.py",
                          "tools/doom/build.py"])


class LinuxAppsTests(unittest.TestCase):
    """The launcher can embed the Linux programs (dash, lua, sqlite3, jq, rg in
    /system/bin) from the Simple tab (CLI or Desktop), the Advanced tab and
    run_demo, building them first."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "linuxapps": True})
            self.assertEqual(env["LAZYOS_LINUXAPPS"], "1")
            self.assertNotIn("LAZYOS_LINUXAPPS",
                             catalog.build_env({**self.base(), "desktop": desktop}))

    def test_simple_mode_offers_it_on_both_interfaces(self) -> None:
        for iface in ("CLI", "Desktop"):
            cfg = catalog.simple_config(demo_config(), "dev", iface, linuxapps=True)
            self.assertTrue(cfg["linuxapps"], iface)
            self.assertFalse(catalog.simple_config(demo_config(), "dev", iface)["linuxapps"])

    def test_advanced_sessions_get_the_card_with_tls_alone(self) -> None:
        self.assertEqual(catalog.net_flags({"net": False, "tls": False}), [])
        self.assertIn("--net", catalog.net_flags({"net": False, "tls": True}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--linuxapps", demo_argv(linuxapps=True, skip_build=False))
        self.assertNotIn("--linuxapps", demo_argv(skip_build=False))
        self.assertNotIn("--linuxapps", demo_argv(linuxapps=True, skip_build=True))

    def test_session_modes_build_the_programs_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "256M", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "linuxapps": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/linuxapps/build.py", "--require"])
        env = plan[at].get("env", {})
        if env:
            self.assertEqual(env.get("LAZYOS_LINUXAPPS"), "1")

    def test_all_optional_apps_build_in_order(self) -> None:
        steps = catalog.app_steps({"lazyrad": True, "modplayer": True, "doom": True,
                                   "linuxapps": True, "tls": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/lazyrad/build.py", "tools/lazyrad/package.py",
                          "tools/doom/build.py", "tools/linuxapps/build.py",
                          "tools/nettls/build.py"])


class TlsTests(unittest.TestCase):
    """The HTTPS clients (`curl`, `wget`, `fetch`; LAZYOS_TLS=1) from the Simple
    tab, the Advanced tab and run_demo: they bring the network stack with them."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_embeds_the_tools_and_the_stack(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "tls": True})
            self.assertEqual(env["LAZYOS_TLS"], "1")
            self.assertEqual(env["LAZYOS_NETD"], "1")
            self.assertEqual(env["LAZYOS_NETD_ARGS"], "demo=0")
            self.assertNotIn("LAZYOS_TLS", catalog.build_env({**self.base(), "desktop": desktop}))

    def test_simple_mode_offers_it_on_both_interfaces_with_networking(self) -> None:
        for iface in ("CLI", "Desktop"):
            cfg = catalog.simple_config(demo_config(), "dev", iface, tls=True)
            self.assertTrue(cfg["tls"], iface)
            self.assertTrue(cfg["net"], iface)
            off = catalog.simple_config(demo_config(), "dev", iface)
            self.assertFalse(off["tls"])
            self.assertFalse(off["net"])

    def test_advanced_sessions_get_the_card_with_tls_alone(self) -> None:
        self.assertEqual(catalog.net_flags({"net": False, "tls": False}), [])
        self.assertIn("--net", catalog.net_flags({"net": False, "tls": True}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--tls", demo_argv(tls=True, skip_build=False))
        self.assertNotIn("--tls", demo_argv(skip_build=False))
        self.assertNotIn("--tls", demo_argv(tls=True, skip_build=True))

    def test_session_modes_build_the_tools_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "256M", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "tls": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/nettls/build.py", "--require"])


if __name__ == "__main__":
    unittest.main()


class JournalTests(unittest.TestCase):
    """An ext2 journal on the OS volume (LAZYOS_JOURNAL=1) from the Advanced
    tab and run_demo."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_build_variable(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "journal": True})
            self.assertEqual(env["LAZYOS_JOURNAL"], "1")
            self.assertNotIn("LAZYOS_JOURNAL",
                             catalog.build_env({**self.base(), "desktop": desktop}))

    def test_simple_mode_leaves_it_off(self) -> None:
        for iface in ("CLI", "Desktop"):
            self.assertFalse(catalog.simple_config(demo_config(), "dev", iface)["journal"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--journal", demo_argv(journal=True, skip_build=False))
        self.assertNotIn("--journal", demo_argv(skip_build=False))
        self.assertNotIn("--journal", demo_argv(journal=True, skip_build=True))
