#!/usr/bin/env python3
"""Launcher tests for extra data asset trees (`LAZYOS_ASSETS`, issue #454):
the Advanced tab's *Asset dirs* field and run_demo's `--assets DIR`.
`test_catalog.py` runs them too.

Run: python tools/lazygui/test_assets.py
"""

from __future__ import annotations

import argparse
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import assets, catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


def parse(argv: list[str]) -> argparse.Namespace:
    """run_demo's assets option (plus `--no-build`) over ``argv``."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--no-build", action="store_true")
    assets.add_assets_option(parser)
    return parser.parse_args(argv)


class AssetDirTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.good, self.other, self.bare = root / "good", root / "other", root / "bare"
        for tree in (self.good, self.other):
            tree.mkdir()
            (tree / assets.MANIFEST).write_text("song.mod | CC0-1.0 | all | made here\n")
        self.bare.mkdir()

    def test_dirs_need_a_manifest_and_must_exist(self) -> None:
        self.assertEqual(assets.check_dirs(f" {self.good} ; "), [str(self.good.resolve())])
        for bad in (str(self.bare), str(Path(self.tmp.name) / "absent")):
            with self.assertRaises(ValueError, msg=bad):
                assets.check_dirs(bad)

    def test_advanced_field_sets_lazyos_assets(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop")
        self.assertNotIn("LAZYOS_ASSETS", catalog.build_env(cfg))
        cfg["assets"] = f"{self.good};{self.other}"
        env = catalog.build_env(cfg)
        self.assertEqual(env["LAZYOS_ASSETS"].split(os.pathsep),
                         [str(self.good.resolve()), str(self.other.resolve())])

    def test_run_demo_gets_one_flag_per_dir(self) -> None:
        argv = demo_argv(skip_build=False, assets=f"{self.good};{self.other}")
        at = argv.index("--assets")
        self.assertEqual(argv[at:at + 4], ["--assets", str(self.good.resolve()),
                                           "--assets", str(self.other.resolve())])
        self.assertNotIn("--assets", demo_argv(skip_build=False))

    def test_skip_build_refuses_asset_dirs(self) -> None:
        with self.assertRaises(ValueError):
            catalog.check_limits(demo_config(skip_build=True, assets=str(self.good)))

    def test_run_demo_option(self) -> None:
        args = parse(["--assets", str(self.good), "--assets", str(self.other)])
        env = assets.build_assets(args)
        self.assertEqual(len(env["LAZYOS_ASSETS"].split(os.pathsep)), 2)
        self.assertEqual(assets.build_assets(parse([])), {})
        with self.assertRaises(ValueError):
            assets.build_assets(parse(["--no-build", "--assets", str(self.good)]))
        with self.assertRaises(ValueError):
            assets.build_assets(parse(["--assets", str(self.bare)]))


if __name__ == "__main__":
    unittest.main()
