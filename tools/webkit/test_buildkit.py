#!/usr/bin/env python3
"""The staged install of ``buildkit.Recipe``: the file list is what the install
step wrote, whatever the prefix already held, and a rebuild leaves nothing of
the old version behind. Runs on a throwaway prefix, no toolchain needed."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import buildkit  # noqa: E402
from toolenv import BuildError  # noqa: E402


class PrefixCase(unittest.TestCase):
    """buildkit pointed at a temporary prefix, stamps and stage."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.saved = (buildkit.PREFIX, buildkit.STAMPS, buildkit.STAGE_ROOT)
        buildkit.PREFIX = root / "prefix"
        buildkit.STAMPS = buildkit.PREFIX / ".stamps"
        buildkit.STAGE_ROOT = root / "stage"
        buildkit.PREFIX.mkdir()

    def tearDown(self) -> None:
        buildkit.PREFIX, buildkit.STAMPS, buildkit.STAGE_ROOT = self.saved
        self.tmp.cleanup()

    def install(self, name: str, key: str, files: dict[str, str]) -> None:
        """What a recipe does: write the files under the staged prefix."""
        with buildkit.Recipe(None, name, key, "1.0"):
            for rel, text in files.items():
                path = buildkit.staged_prefix() / rel
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)

    def stamp(self, name: str) -> dict:
        return json.loads((buildkit.STAMPS / f"{name}.json").read_text())


class StagedInstallTests(PrefixCase):
    def test_a_first_install_lists_what_it_wrote(self) -> None:
        self.install("zlib", "k1", {"lib/libz.a": "a", "include/zlib.h": "h"})
        self.assertEqual(self.stamp("zlib")["files"], ["include/zlib.h", "lib/libz.a"])
        self.assertEqual((buildkit.PREFIX / "lib" / "libz.a").read_text(), "a")

    def test_a_rebuild_over_an_existing_prefix_still_lists_its_files(self) -> None:
        # The review's case: every file already exists, so a before/after
        # difference of the prefix is empty. The staged list is not.
        files = {"lib/libz.a": "a", "include/zlib.h": "h"}
        self.install("zlib", "k1", files)
        self.install("zlib", "k2", files)
        self.assertEqual(self.stamp("zlib")["files"], ["include/zlib.h", "lib/libz.a"])
        self.assertEqual(self.stamp("zlib")["key"], "k2")

    def test_a_rebuild_removes_the_files_of_the_old_version(self) -> None:
        self.install("zlib", "k1", {"lib/libz.a": "old", "lib/gone.a": "x", "share/old/doc.txt": "x"})
        self.install("zlib", "k2", {"lib/libz.a": "new", "lib/extra.a": "y"})
        self.assertEqual(self.stamp("zlib")["files"], ["lib/extra.a", "lib/libz.a"])
        self.assertFalse((buildkit.PREFIX / "lib" / "gone.a").exists())
        self.assertFalse((buildkit.PREFIX / "share").exists(), "the directory it emptied goes too")
        self.assertEqual((buildkit.PREFIX / "lib" / "libz.a").read_text(), "new")

    def test_a_rebuild_leaves_other_libraries_alone(self) -> None:
        self.install("zlib", "k1", {"lib/libz.a": "z", "lib/pkgconfig/zlib.pc": "z"})
        self.install("libpng", "p1", {"lib/libpng.a": "p", "lib/pkgconfig/libpng.pc": "p"})
        self.install("zlib", "k2", {"lib/libz.a": "z2"})
        self.assertEqual(self.stamp("libpng")["files"], ["lib/libpng.a", "lib/pkgconfig/libpng.pc"])
        self.assertTrue((buildkit.PREFIX / "lib" / "pkgconfig" / "libpng.pc").is_file())
        self.assertFalse((buildkit.PREFIX / "lib" / "pkgconfig" / "zlib.pc").exists())

    def test_the_build_sees_only_what_it_depends_on(self) -> None:
        self.install("zlib", "k1", {"lib/libz.a": "z"})
        seen = []
        with buildkit.Recipe(None, "zlib", "k2"):
            seen.append((buildkit.PREFIX / "lib" / "libz.a").exists())
        self.assertEqual(seen, [False], "its own old files are gone while it builds")

    def test_a_failed_build_leaves_no_stamp_and_no_stage(self) -> None:
        self.install("zlib", "k1", {"lib/libz.a": "z"})
        with self.assertRaises(BuildError):
            with buildkit.Recipe(None, "zlib", "k2"):
                (buildkit.staged_prefix()).mkdir(parents=True)
                raise BuildError("compiler exploded")
        self.assertIsNone(buildkit.stamp_key("zlib"), "a failed rebuild must not look finished")
        self.assertFalse((buildkit.STAGE_ROOT / "zlib").exists())
        self.assertFalse(buildkit.is_current("zlib", "k1"))

    def test_only_the_staged_prefix_is_installed(self) -> None:
        with buildkit.Recipe(None, "fontconfig", "k1"):
            outside = buildkit.stage() / "system" / "etc" / "fonts" / "fonts.conf"
            outside.parent.mkdir(parents=True)
            outside.write_text("conf")
            inside = buildkit.staged_prefix() / "lib" / "libfontconfig.a"
            inside.parent.mkdir(parents=True)
            inside.write_text("a")
        self.assertEqual(self.stamp("fontconfig")["files"], ["lib/libfontconfig.a"])
        self.assertFalse((buildkit.PREFIX / "system").exists())

    def test_symlinks_are_listed_and_kept(self) -> None:
        with buildkit.Recipe(None, "skia", "k1"):
            real = buildkit.staged_prefix() / "skia" / "include"
            real.mkdir(parents=True)
            (real / "a.h").write_text("h")
            link = buildkit.staged_prefix() / "include" / "skia"
            link.parent.mkdir(parents=True)
            try:
                link.symlink_to("../skia/include")
            except OSError:
                self.skipTest("no symlinks here")
        self.assertIn("include/skia", self.stamp("skia")["files"])
        self.assertTrue((buildkit.PREFIX / "include" / "skia").is_symlink())

    def test_the_stamp_carries_the_pin_and_the_licence(self) -> None:
        import wkpins
        dep = wkpins.BY_NAME["openssl"]
        with buildkit.Recipe(dep, "openssl", "k1"):
            (buildkit.staged_prefix() / "lib").mkdir(parents=True)
            (buildkit.staged_prefix() / "lib" / "libssl.a").write_text("s")
        record = self.stamp("openssl")
        self.assertEqual((record["license"], record["sha256"]), (dep.license, dep.source.sha256))
        self.assertIn("LazyWeb", record["notes"])

    def test_staging_outside_a_recipe_is_an_error(self) -> None:
        with self.assertRaises(BuildError):
            buildkit.stage()


class ListTreeTests(PrefixCase):
    def test_the_stamps_are_not_files_of_a_library(self) -> None:
        (buildkit.STAMPS).mkdir()
        (buildkit.STAMPS / "x.json").write_text("{}")
        (buildkit.PREFIX / "lib").mkdir()
        (buildkit.PREFIX / "lib" / "a.a").write_text("a")
        self.assertEqual(buildkit.list_tree(buildkit.PREFIX), ["lib/a.a"])

    def test_a_missing_directory_is_empty(self) -> None:
        self.assertEqual(buildkit.list_tree(buildkit.PREFIX / "nothing"), [])


if __name__ == "__main__":
    unittest.main()
