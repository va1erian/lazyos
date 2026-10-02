#!/usr/bin/env python3
"""Tests of the core package build (`tools/xui/core_packages.py`, issue #509).

    python tools/xui/test_core_packages.py
"""

from __future__ import annotations

import hashlib
import random
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import core_packages  # noqa: E402

sys.path.insert(0, str(core_packages.ROOT / "tools" / "pkg"))
import pkgmanifest  # noqa: E402

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11
    tomllib = None


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class CorePackageTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.xui = self.dir / "xui"
        self.xui.mkdir()
        # Stand-ins for the built programs: different bytes per app.
        for short, (elf, _optional) in core_packages.CORE_APPS.items():
            (self.xui / elf).write_bytes(b"\x7fELF " + short.encode() * 1000)

    def test_every_core_app_has_a_source_tree_and_nothing_else_does(self) -> None:
        trees = sorted(p.name for p in core_packages.SOURCES.iterdir() if p.is_dir())
        self.assertEqual(trees, sorted(core_packages.CORE_APPS))
        for kept_out in ("terminal", "installer", "devices", "shell"):
            self.assertNotIn(kept_out, core_packages.CORE_APPS)

    @unittest.skipIf(tomllib is None, "needs Python 3.11+")
    def test_every_source_manifest_is_valid_and_names_its_app(self) -> None:
        for short in core_packages.CORE_APPS:
            text = (core_packages.SOURCES / short / "manifest.toml").read_text(encoding="utf-8")
            manifest = tomllib.loads(text)
            self.assertEqual(pkgmanifest.validate_manifest(manifest), [], short)
            self.assertEqual(manifest["app"]["system_name"], f"os.lazy.{short}")
            self.assertEqual(manifest["entry"]["binary"], f"bin/{short}.elf")
            self.assertEqual(manifest["entry"]["args"], ["--client"])
            self.assertEqual(manifest["entry"]["abi"], "linux")
            self.assertIn("os.lazy.display.v1", manifest["permissions"]["interfaces"])
            self.assertTrue((core_packages.SOURCES / short / "docs" / "README.md").is_file())

    def test_the_manifest_takes_the_workspace_version_and_the_autostart_flag(self) -> None:
        text = 'version = "0.1.0"\nautostart = false\n'
        self.assertEqual(core_packages.render_manifest(text, "1.2.3", True),
                         'version = "1.2.3"\nautostart = true\n')
        with self.assertRaises(core_packages.CoreError):
            core_packages.render_manifest('version = "0.1.0"\n', "1.0.0", False)
        self.assertRegex(core_packages.workspace_version(), r"^\d+\.\d+")

    def test_builds_are_reproducible_and_autostart_changes_the_digest(self) -> None:
        first = core_packages.build_core_packages(self.xui, self.dir / "a", "0.1.0")
        second = core_packages.build_core_packages(self.xui, self.dir / "b", "0.1.0")
        self.assertEqual(len(first), len(core_packages.CORE_APPS))
        for one, two in zip(first, second):
            self.assertEqual(one.name, two.name)
            self.assertEqual(sha(one), sha(two), f"{one.name} is not reproducible")
            auto = one.parent / core_packages.AUTOSTART_DIR / one.name
            self.assertNotEqual(sha(one), sha(auto))
            self.assertLessEqual(one.stat().st_size, core_packages.MAX_PACKAGE_FILE)
        listing = (self.dir / "a" / core_packages.LIST).read_text(encoding="utf-8")
        line = next(l for l in listing.splitlines() if l.startswith("editor "))
        short, name, version, plain, digest, auto, auto_digest = line.split()
        self.assertEqual((name, version), ("os.lazy.editor", "0.1.0"))
        self.assertEqual(digest, sha(self.dir / "a" / plain))
        self.assertEqual(auto_digest, sha(self.dir / "a" / auto))

    def test_a_missing_optional_app_is_skipped_and_a_mandatory_one_fails(self) -> None:
        (self.xui / "xui-docs.elf").unlink()
        built = core_packages.build_core_packages(self.xui, self.dir / "c", "0.1.0")
        self.assertNotIn("os.lazy.docs-0.1.0.lzp", [p.name for p in built])
        (self.xui / "xui-paint.elf").unlink()
        with self.assertRaises(core_packages.CoreError):
            core_packages.build_core_packages(self.xui, self.dir / "d", "0.1.0")

    def test_stale_archives_are_removed(self) -> None:
        out = self.dir / "e"
        (out / core_packages.AUTOSTART_DIR).mkdir(parents=True)
        stale = out / "os.lazy.gone-0.0.1.lzp"
        stale.write_bytes(b"old")
        core_packages.build_core_packages(self.xui, out, "0.1.0")
        self.assertFalse(stale.exists())

    def test_an_oversized_package_is_an_error(self) -> None:
        # Noise does not deflate, so the archive is as large as the program.
        noise = random.Random(7).randbytes(core_packages.MAX_PACKAGE_FILE + 4096)
        (self.xui / "xui-paint.elf").write_bytes(noise)
        with self.assertRaises(core_packages.CoreError):
            core_packages.build_core_packages(self.xui, self.dir / "f", "0.1.0")


if __name__ == "__main__":
    unittest.main()
