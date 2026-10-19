#!/usr/bin/env python3
"""The U2a judge fails when it should (docs/update-plan.md decision 6).

Run: python tools/install/test_grubenv_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import grubenv_judge as g  # noqa: E402

LAZYOS = "GRUB\nBOOT:MEDIA:uefi\nfs: root x is ahci0p3\n"
LINUX = "GRUB\nLINUX:STANDIN\n"


def env(value: str) -> bytes:
    return g.env_block({"lazyos_boots": value})


class JudgeTests(unittest.TestCase):
    def test_block_roundtrip(self) -> None:
        self.assertEqual(len(env("4")), 1024)
        self.assertEqual(g.counter(env("4")), "4")
        self.assertEqual(g.counter(env("x")), "x")
        self.assertIsNone(g.counter(b"# GRUB Environment Block\n" + b"#" * 10))
        self.assertIsNone(g.counter(b"nope" + env("4")[4:]))
        self.assertIsNone(g.counter(g.env_block({"saved_entry": "linux"})))
        with self.assertRaises(ValueError):
            g.env_block({"a": "b" * 2000})

    def test_good_boots_pass(self) -> None:
        self.assertEqual(g.judge_boot(LAZYOS, env("3"), "lazyos", "3"), [])
        self.assertEqual(g.judge_boot(LINUX, env("0"), "linux", "0"), [])
        self.assertEqual(g.judge_boot(LINUX, env("x"), "linux", "x"), [])

    def test_wrong_entry_or_counter_fails(self) -> None:
        self.assertTrue(g.judge_boot(LINUX, env("3"), "lazyos", "3"))
        self.assertTrue(g.judge_boot(LAZYOS, env("0"), "linux", "0"))
        self.assertTrue(g.judge_boot(LAZYOS + LINUX, env("3"), "lazyos", "3"))
        self.assertTrue(g.judge_boot("", env("3"), "lazyos", "3"))
        self.assertTrue(g.judge_boot(LAZYOS, env("4"), "lazyos", "3"))
        self.assertTrue(g.judge_boot(LAZYOS, g.env_block({}), "lazyos", "3"))
        self.assertTrue(g.judge_boot(LAZYOS, env("3")[:512], "lazyos", "3"))

    def test_torn_block_has_no_counter(self) -> None:
        bad = (b"# GRUB Environment Blocx" + bytes([10]) + b"lazyos_boots=4" + bytes([10])).ljust(1024, b"#")
        self.assertEqual(g.judge_boot(LINUX, bad, "linux", None), [])
        self.assertTrue(g.judge_boot(LINUX, env("4"), "linux", None))

    def test_ladders_are_consistent(self) -> None:
        for ladder in (g.COUNTDOWN, g.GRUBD):
            for (before, entry, after), nxt in zip(ladder, ladder[1:]):
                self.assertEqual(after, nxt[0])
        for before, entry, after in g.CORRUPT:
            self.assertEqual((entry, after), ("linux", before))

    def test_linux_side(self) -> None:
        self.assertEqual(g.judge_linux_side({"a": b"1"}, {"a": b"1"}), [])
        self.assertTrue(g.judge_linux_side({"a": b"1"}, {"a": b"2"}))
        self.assertTrue(g.judge_linux_side({"a": b"1"}, {}))

    def test_splice(self) -> None:
        cfg = "set default=0\nif [ -f ${config_directory}/custom.cfg ]; then\n  source x\nfi\n"
        custom = f"# c\n{g.BEGIN}\nsnippet\n{g.END}\n"
        new_cfg, new_custom = g.splice_grubd(cfg, custom)
        self.assertEqual(new_custom, "# c\n")
        self.assertLess(new_cfg.index("snippet"), new_cfg.index("source x"))
        self.assertGreater(new_cfg.index("snippet"), new_cfg.index("default=0"))
        with self.assertRaises(ValueError):
            g.splice_grubd(cfg, "# c\n")
        with self.assertRaises(ValueError):
            g.splice_grubd("set default=0\n", custom)

    def test_grub_errors(self) -> None:
        self.assertEqual(g.grub_errors("a\nerror: no such device: x.\nb"), ["error: no such device: x."])


if __name__ == "__main__":
    unittest.main()
