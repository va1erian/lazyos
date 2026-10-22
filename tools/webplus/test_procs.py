#!/usr/bin/env python3
"""The benches' names for the browser's four processes (`procs.py`)."""

from __future__ import annotations

import sys
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import procs  # noqa: E402

PACKAGES = procs.ROOT / "xui-app" / "packages"


def binary(package: str) -> str:
    manifest = tomllib.loads((PACKAGES / package / "manifest.toml").read_text(encoding="utf-8"))
    return Path(manifest["entry"]["binary"]).name


class Names(unittest.TestCase):
    def test_the_programs_are_the_packages_binaries(self) -> None:
        # The kernel's PERF:CPU `comm=` is the program's basename; if a manifest renames it, this says so.
        self.assertEqual(binary("lazywebplus"), procs.CLIENT_PROGRAM)
        self.assertEqual(binary("webd"), procs.WEBD_PROGRAM)

    def test_a_comm_names_its_role(self) -> None:
        self.assertEqual(procs.comm_role(procs.CLIENT_PROGRAM), procs.CLIENT)
        self.assertEqual(procs.comm_role(procs.WEBD_PROGRAM), procs.WEBD)
        self.assertEqual(procs.comm_role("webd"), procs.WEBD)
        self.assertEqual(procs.comm_role("WebProcess"), procs.WEB)
        self.assertEqual(procs.comm_role("networkprocess"), procs.NETWORK)
        for other in ("xuid", "fork", "webdemo.elf", "lazywebplus-app"):
            self.assertIsNone(procs.comm_role(other), other)

    def test_the_engines_own_pids_win_over_a_comm(self) -> None:
        text = ("WEBD:NOTE:WEBPROCESS:UP:39\nWEBD:NOTE:NETWORKPROCESS:UP:41\n"
                "PERF:CPU tgid=39 pct=62 user=60 kernel=2 class=normal demotions=3 comm=fork\n"
                "PERF:CPU tgid=41 pct=30 user=20 kernel=10 class=normal demotions=0 comm=networkprocess\n"
                "PERF:CPU tgid=7 pct=25 user=20 kernel=5 class=normal demotions=0 comm=webd.elf\n"
                "PERF:CPU tgid=8 pct=22 user=20 kernel=2 class=normal demotions=0 comm=lazywebplus.elf\n"
                "PERF:CPU tgid=9 pct=21 user=9 kernel=12 class=normal demotions=0 comm=xuid\n")
        rows = procs.cpu_rows(text)
        self.assertEqual(sorted(rows), sorted([procs.WEB, procs.NETWORK, procs.WEBD, procs.CLIENT, "xuid"]))
        self.assertEqual(rows[procs.WEB], [(62, 60, 2)])
        # The old prefix is not the engine's: the pid is then only a comm.
        old = procs.cpu_rows(text.replace("WEBD:NOTE:", "WEBPLUS:"))
        self.assertIn("fork", old)


if __name__ == "__main__":
    unittest.main()
