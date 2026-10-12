#!/usr/bin/env python3
"""``registry.ensure`` (the stamp logic that skips finished libraries) and
``buildkit.key_of`` (what a stamp must equal). Fake libraries on a temporary
prefix: no toolchain, no download."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import buildkit  # noqa: E402
import registry  # noqa: E402
import wkpins  # noqa: E402
from test_buildkit import PrefixCase  # noqa: E402


class KeyTests(PrefixCase):
    def key(self, sha="s", config=None, deps=()):
        return buildkit.key_of(sha, config or {"a": 1}, list(deps))

    def test_the_same_inputs_give_the_same_key(self) -> None:
        self.assertEqual(self.key(), self.key())
        self.assertEqual(len(self.key()), 20)

    def test_config_order_does_not_matter(self) -> None:
        self.assertEqual(self.key(config={"a": 1, "b": 2}), self.key(config={"b": 2, "a": 1}))

    def test_each_input_changes_the_key(self) -> None:
        base = self.key()
        self.assertNotEqual(base, self.key(sha="t"))
        self.assertNotEqual(base, self.key(config={"a": 2}))
        saved = buildkit.RECIPE_REV
        try:
            buildkit.RECIPE_REV = "2"
            self.assertNotEqual(base, self.key())
        finally:
            buildkit.RECIPE_REV = saved

    def test_a_dependency_that_is_not_installed_is_part_of_the_key(self) -> None:
        missing = self.key(deps=["zlib"])
        self.install("zlib", "z1", {"lib/libz.a": "z"})
        self.assertNotEqual(missing, self.key(deps=["zlib"]))

    def test_a_rebuilt_dependency_changes_its_dependents_key(self) -> None:
        self.install("zlib", "z1", {"lib/libz.a": "z"})
        before = self.key(deps=["zlib"])
        self.install("zlib", "z2", {"lib/libz.a": "z"})
        self.assertNotEqual(before, self.key(deps=["zlib"]))


class EnsureTests(PrefixCase):
    def setUp(self) -> None:
        super().setUp()
        self.built: list[str] = []
        self.saved_libs = dict(registry.LIBS)
        registry.LIBS.clear()
        self.config = {"zlib": {"v": 1}, "png": {"v": 1}}
        self.add("zlib", [])
        self.add("png", ["zlib"])

    def tearDown(self) -> None:
        registry.LIBS.clear()
        registry.LIBS.update(self.saved_libs)
        super().tearDown()

    def add(self, name: str, needs: list[str]) -> None:
        def build(tree: Path, jobs: int) -> None:
            self.built.append(name)
            path = buildkit.staged_prefix() / "lib" / f"lib{name}.a"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"{name} {self.config[name]}")

        # The config is read when ensure() keys the library, as in the real table.
        lib = registry.Lib(name, needs, self.config[name], build, own_tree=True,
                           extra={"source_sha": f"sha-{name}"})
        registry.register(lib)

    def ensure(self, name: str) -> None:
        registry.ensure(name, 1)

    def test_a_dependency_is_built_first_and_once(self) -> None:
        self.ensure("png")
        self.assertEqual(self.built, ["zlib", "png"])

    def test_a_current_library_is_skipped(self) -> None:
        self.ensure("png")
        self.ensure("png")
        self.assertEqual(self.built, ["zlib", "png"])

    def test_changing_a_config_rebuilds_it_and_what_needs_it(self) -> None:
        self.ensure("png")
        self.config["zlib"] = {"v": 2}
        registry.LIBS["zlib"].config = self.config["zlib"]
        self.ensure("png")
        self.assertEqual(self.built, ["zlib", "png", "zlib", "png"])

    def test_changing_a_leaf_rebuilds_only_the_leaf(self) -> None:
        self.ensure("png")
        registry.LIBS["png"].config = {"v": 2}
        self.ensure("png")
        self.assertEqual(self.built, ["zlib", "png", "png"])

    def test_a_missing_stamp_rebuilds(self) -> None:
        self.ensure("zlib")
        (buildkit.STAMPS / "zlib.json").unlink()
        self.ensure("zlib")
        self.assertEqual(self.built, ["zlib", "zlib"])

    def test_the_done_set_keeps_a_diamond_to_one_build(self) -> None:
        self.config["freetype"] = {"v": 1}
        self.add("freetype", ["zlib", "png"])
        done: set[str] = set()
        registry.ensure("freetype", 1, done)
        self.assertEqual(sorted(self.built), ["freetype", "png", "zlib"])
        self.assertEqual(self.built.count("zlib"), 1)

    def test_a_failed_build_is_retried(self) -> None:
        def broken(tree: Path, jobs: int) -> None:
            raise buildkit.BuildError("no")

        registry.LIBS["zlib"].build = broken
        with self.assertRaises(buildkit.BuildError):
            self.ensure("zlib")
        self.assertIsNone(buildkit.stamp_key("zlib"))
        registry.LIBS["zlib"].build = lambda tree, jobs: self.built.append("zlib")
        self.ensure("zlib")
        self.assertEqual(self.built, ["zlib"])

    def test_a_pinned_library_is_keyed_by_its_archive_digest(self) -> None:
        # A library with a pin takes the key from wkpins, not from `extra`.
        dep = wkpins.BY_NAME["zlib"]
        key_a = buildkit.key_of(dep.source.sha256, {"config": self.config["zlib"], "patches": []}, [])
        registry.LIBS["zlib"].extra = {"source_sha": "ignored"}
        registry.LIBS["zlib"].own_tree = False
        original = buildkit.source_tree
        buildkit.source_tree = lambda d: Path()
        try:
            self.ensure("zlib")
        finally:
            buildkit.source_tree = original
        self.assertEqual(buildkit.stamp_key("zlib"), key_a)


if __name__ == "__main__":
    unittest.main()
