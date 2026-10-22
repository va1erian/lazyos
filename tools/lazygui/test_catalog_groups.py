#!/usr/bin/env python3
"""Launcher tests for the Simple tab's feature groups (Games, Multimedia, Dev,
Networking, Web) and how they map onto `simple_config`'s switches."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from lazygui import catalog  # noqa: E402
from lazygui.simple import SIMPLE_EXTRAS  # noqa: E402
from lazygui.test_catalog import demo_config  # noqa: E402


def desktop(*groups: str, iface: str = "Desktop", **options) -> dict:
    return catalog.simple_config(demo_config(), "dev", iface,
                                 **catalog.group_switches(set(groups)), **options)


class SimpleGroups(unittest.TestCase):
    def test_groups_and_options_name_real_switches(self) -> None:
        names = {n for _k, _l, _d, group in catalog.SIMPLE_GROUPS for n in group}
        self.assertTrue(names | set(catalog.SIMPLE_OPTIONS) <= set(SIMPLE_EXTRAS))
        self.assertEqual(set(catalog.group_switches(set())), names | {"pictures"})

    def test_the_picture_viewer_is_always_on_a_desktop(self) -> None:
        self.assertTrue(desktop()["pictures"])
        self.assertFalse(desktop(iface="CLI")["pictures"])

    def test_nothing_chosen_adds_nothing(self) -> None:
        cfg = desktop()
        for name in ("doom", "quake", "lazyrad", "emusic", "modplayer", "linuxapps",
                     "traydemo", "games", "net", "tls", "mail"):
            self.assertFalse(cfg[name], name)

    def test_games_have_doom_and_the_lazyrad_brickbreaker(self) -> None:
        cfg = desktop("games")
        self.assertTrue(cfg["doom"] and cfg["quake"] and cfg["srb2kart"] and cfg["lazyrad"])
        self.assertTrue(cfg["games"], "LazyGolf and the NES Player")
        self.assertIn("LAZYOS_DOOM", catalog.build_env(cfg))
        self.assertIn("lazyrad-os/samples/brickbreaker", catalog.build_env(cfg)["LAZYRAD_SAMPLES"])

    def test_multimedia_has_emusic_and_the_mod_player(self) -> None:
        cfg = desktop("multimedia")
        self.assertTrue(cfg["emusic"] and cfg["modplayer"])

    def test_dev_has_lazyrad_and_the_linux_programs_even_on_the_cli(self) -> None:
        self.assertTrue(desktop("dev")["lazyrad"] and desktop("dev")["linuxapps"])
        cli = desktop("dev", iface="CLI")
        self.assertTrue(cli["linuxapps"])
        self.assertFalse(cli["lazyrad"])

    def test_networking_has_the_stack_and_https(self) -> None:
        cfg = desktop("networking")
        self.assertTrue(cfg["net"] and cfg["tls"])
        self.assertTrue(desktop("networking", iface="CLI")["tls"])

    def test_web_has_mail_and_lazysky_over_networking(self) -> None:
        cfg = desktop("web")
        self.assertTrue(cfg["lazysky"] and cfg["mail"] and cfg["tls"] and cfg["net"])
        self.assertFalse(cfg["lazywebplus"], "LazyWeb+ is its own group (a Docker build)")

    def test_lazywebplus_is_its_own_group_over_networking(self) -> None:
        cfg = desktop("lazywebplus")
        self.assertTrue(cfg["lazywebplus"] and cfg["net"])
        self.assertFalse(cfg["lazysky"] or cfg["mail"])
        self.assertFalse(desktop("lazywebplus", iface="CLI")["lazywebplus"], "a browser needs the desktop")


if __name__ == "__main__":
    unittest.main()
