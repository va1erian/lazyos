#!/usr/bin/env python3
"""Launcher tests for the optional apps an image can embed: Doom, the LazyRAD
MOD player, the Linux programs, the HTTPS tools, the SMB client, LazyWeb, Mail and the tray demo (from the Simple tab, the Advanced tab and
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


class EmusicTests(unittest.TestCase):
    """The launcher can put the emusic package on the image
    (`/system/share/samples/emusic.lzp`), from both tabs and run_demo."""

    base = DoomTests.base

    def test_the_switch_sets_the_embed_variable(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "emusic": True})
        self.assertEqual(env["LAZYOS_EMUSIC"], "1")
        self.assertNotIn("LAZYOS_EMUSIC", catalog.build_env({**self.base(), "desktop": True}))

    def test_simple_desktop_can_include_it_and_cli_cannot(self) -> None:
        self.assertTrue(catalog.simple_config(demo_config(), "dev", "Desktop", emusic=True)["emusic"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI", emusic=True)["emusic"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["emusic"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--emusic", demo_argv(emusic=True, skip_build=False))
        self.assertNotIn("--emusic", demo_argv(skip_build=False))
        self.assertNotIn("--emusic", demo_argv(emusic=True, skip_build=True))

    def test_session_modes_build_the_package_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "1G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "emusic": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(labels[at - 1], "Build emusic package")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/emusic/build.py", "--require"])

    def test_it_builds_after_doom(self) -> None:
        steps = catalog.app_steps({"doom": True, "emusic": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/doom/build.py", "tools/emusic/build.py"])


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


class MailTests(unittest.TestCase):
    """Mail (esMail over TLS; LAZYOS_MAIL=1) from the Simple tab, the Advanced
    tab and run_demo: a desktop app that brings HTTPS and the network with it."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_embeds_mail_on_the_desktop_only(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "tls": True, "mail": True})
        self.assertEqual(env["LAZYOS_MAIL"], "1")
        self.assertEqual(env["LAZYOS_TLS"], "1")
        self.assertNotIn("LAZYOS_MAIL", catalog.build_env({**self.base(), "desktop": True}))
        self.assertNotIn("LAZYOS_MAIL", catalog.build_env({**self.base(), "mail": True}))

    def test_simple_desktop_gets_mail_with_https_and_cli_does_not(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", mail=True)
        self.assertTrue(cfg["mail"] and cfg["tls"] and cfg["net"])
        cli = catalog.simple_config(demo_config(), "dev", "CLI", mail=True)
        self.assertFalse(cli["mail"] or cli["tls"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["mail"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--mail", demo_argv(mail=True, desktop=True, skip_build=False))
        self.assertNotIn("--mail", demo_argv(mail=True, desktop=False, skip_build=False))
        self.assertNotIn("--mail", demo_argv(mail=True, desktop=True, skip_build=True))

    def test_session_modes_build_mail_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "256M", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "desktop": True,
               "tls": True, "mail": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/xui/build.py", "--mail"])


class LazyWebTests(unittest.TestCase):
    """The LazyWeb browser (LAZYOS_LAZYWEB=1, docs/lazyweb.md) from the Simple
    tab, the Advanced tab and run_demo: a desktop app that brings the network
    stack and the HTTPS tools."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_brings_the_desktop_the_stack_and_https(self) -> None:
        # The Advanced checkbox alone: the image build refuses LAZYOS_LAZYWEB
        # without the desktop and the stack, so the plan sets them.
        env = catalog.build_env({**self.base(), "lazyweb": True})
        for name in ("LAZYOS_LAZYWEB", "LAZYOS_DESKTOP", "LAZYOS_NETD", "LAZYOS_TLS"):
            self.assertEqual(env.get(name), "1", name)
        self.assertEqual(env["LAZYOS_NETD_ARGS"], "demo=0")
        off = catalog.build_env({**self.base(), "desktop": True})
        self.assertNotIn("LAZYOS_LAZYWEB", off)
        self.assertNotIn("LAZYOS_TLS", off)

    def test_simple_desktop_offers_it_and_cli_does_not(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", lazyweb=True)
        self.assertTrue(cfg["lazyweb"])
        self.assertTrue(cfg["tls"])
        self.assertTrue(cfg["net"])
        cli = catalog.simple_config(demo_config(), "dev", "CLI", lazyweb=True)
        self.assertFalse(cli["lazyweb"])
        self.assertFalse(cli["tls"])
        self.assertFalse(cli["net"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["lazyweb"])

    def test_the_simple_tab_passes_it_by_name(self) -> None:
        # `ui` calls simple_config positionally in SIMPLE_EXTRAS order.
        import inspect
        from lazygui.simple import SIMPLE_EXTRAS
        params = list(inspect.signature(catalog.simple_config).parameters)[3:]
        self.assertEqual(params, list(SIMPLE_EXTRAS))

    def test_sessions_get_the_card_with_lazyweb_alone(self) -> None:
        self.assertIn("--net", catalog.net_flags({"net": False, "tls": False, "lazyweb": True}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--lazyweb", demo_argv(lazyweb=True, skip_build=False))
        self.assertNotIn("--lazyweb", demo_argv(skip_build=False))
        self.assertNotIn("--lazyweb", demo_argv(lazyweb=True, skip_build=True))
        self.assertIn("--net", demo_argv(lazyweb=True, skip_build=False))

    def test_simple_start_plans_the_browser(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", lazyweb=True)
        argv = catalog.build_plan(cfg)[-1]["argv"]
        self.assertIn("--lazyweb", argv)
        self.assertIn("--tls", argv)

    def test_session_modes_build_the_browser_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "1G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "lazyweb": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/xui/build.py"])
        self.assertIn("--net", plan[-1]["argv"])


class TrayDemoTests(unittest.TestCase):
    """The tray sample app (LAZYOS_TRAYDEMO=1, docs/tray-plan.md T1) from the
    Simple tab, the Advanced tab, run_demo and its session script."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_ships_it_on_the_desktop_only(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "traydemo": True})
        self.assertEqual(env["LAZYOS_TRAYDEMO"], "1")
        self.assertNotIn("LAZYOS_TRAYDEMO", catalog.build_env({**self.base(), "desktop": True}))
        self.assertNotIn("LAZYOS_TRAYDEMO", catalog.build_env({**self.base(), "traydemo": True}))

    def test_simple_desktop_gets_it_and_cli_does_not(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", traydemo=True)
        self.assertTrue(cfg["traydemo"])
        self.assertEqual(catalog.build_env({**self.base(), **cfg})["LAZYOS_TRAYDEMO"], "1")
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI", traydemo=True)["traydemo"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["traydemo"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--traydemo", demo_argv(traydemo=True, desktop=True, skip_build=False))
        self.assertNotIn("--traydemo", demo_argv(traydemo=True, desktop=False, skip_build=False))
        self.assertNotIn("--traydemo", demo_argv(traydemo=True, desktop=True, skip_build=True))

    def test_the_tray_session_asks_for_the_app_and_the_ui_probe(self) -> None:
        index = {entry[0]: i for i, entry in enumerate(catalog.SCRIPTS)}
        self.assertEqual(catalog.SCRIPTS[index["tray.json"]][2:], (("desktop",), "term"))
        cfg = {**self.base(), "mode": "Scripted session", "desktop": True,
               "xui_autostart": "term", "script": index["tray.json"]}
        env = catalog.build_env(cfg)
        self.assertEqual((env["LAZYOS_TRAYDEMO"], env["LAZYOS_UI_PROBE"]), ("1", "1"))
        # Another script, or the same index outside a session, adds nothing.
        other = catalog.build_env({**cfg, "script": index["xui_settings.json"]})
        self.assertNotIn("LAZYOS_UI_PROBE", other)
        self.assertNotIn("LAZYOS_UI_PROBE", catalog.build_env({**cfg, "mode": "Interactive demo"}))

    def test_no_normal_session_launches_it(self) -> None:
        # Only `LAZYOS_TRAYDEMO=1` images ship it: `core_apps.json` must not wait for it.
        path = os.path.join(catalog.ROOT, "tools", "screenshot", "examples", "core_apps.json")
        with open(path, encoding="utf-8") as script:
            self.assertNotIn("os.lazy.traydemo", script.read())



class PictureViewerTests(unittest.TestCase):
    """The Picture Viewer (LAZYOS_PICTURES=1, docs/lazyrad-pictures.md) from the
    Simple tab, the Advanced tab and run_demo, with the player built first."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_ships_it_on_the_desktop_only(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "pictures": True})
        self.assertEqual(env["LAZYOS_PICTURES"], "1")
        self.assertNotIn("LAZYOS_PICTURES", catalog.build_env({**self.base(), "desktop": True}))
        self.assertNotIn("LAZYOS_PICTURES", catalog.build_env({**self.base(), "pictures": True}))

    def test_simple_desktop_gets_it_and_cli_does_not(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", pictures=True)
        self.assertTrue(cfg["pictures"])
        self.assertEqual(catalog.build_env({**self.base(), **cfg})["LAZYOS_PICTURES"], "1")
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI", pictures=True)["pictures"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["pictures"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--pictures", demo_argv(pictures=True, desktop=True, skip_build=False))
        self.assertNotIn("--pictures", demo_argv(pictures=True, desktop=False, skip_build=False))
        self.assertNotIn("--pictures", demo_argv(pictures=True, desktop=True, skip_build=True))

    def test_a_plain_build_builds_the_player_first(self) -> None:
        steps = catalog.app_steps({"pictures": True, "desktop": True})
        self.assertEqual([step["argv"][1:] for step in steps], [["tools/lazyrad/build.py"]])
        self.assertEqual(catalog.app_steps({"pictures": True}), [], "a CLI image has no viewer")
        both = catalog.app_steps({"pictures": True, "desktop": True, "lazyrad": True})
        self.assertEqual(len(both), 1, "one LazyRAD build serves both")

    def test_the_session_asks_for_the_viewer_and_the_ui_probe(self) -> None:
        index = {entry[0]: i for i, entry in enumerate(catalog.SCRIPTS)}
        cfg = {**self.base(), "mode": "Scripted session", "desktop": True,
               "xui_autostart": "term", "script": index["lazyrad_pictures.json"]}
        env = catalog.build_env(cfg)
        self.assertEqual((env["LAZYOS_PICTURES"], env["LAZYOS_UI_PROBE"]), ("1", "1"))


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


class SmbTests(unittest.TestCase):
    """The SMB 2.1 client `smb` (docs/smb-plan.md F2) from the Advanced tab and
    run_demo: its switch, the network stack it needs, and the run_demo flag."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_build_variable_and_the_stack(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "smb": True})
            self.assertEqual(env["LAZYOS_SMB"], "1")
            self.assertEqual(env["LAZYOS_NETD"], "1", "smb connects through the socket service")
            self.assertNotIn("LAZYOS_SMB", catalog.build_env({**self.base(), "desktop": desktop}))

    def test_advanced_sessions_get_the_card_with_smb_alone(self) -> None:
        self.assertEqual(catalog.net_flags({"net": False, "smb": False}), [])
        self.assertIn("--net", catalog.net_flags({"net": False, "smb": True}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--smb", demo_argv(smb=True, skip_build=False))
        self.assertNotIn("--smb", demo_argv(skip_build=False))
        self.assertNotIn("--smb", demo_argv(smb=True, skip_build=True))
