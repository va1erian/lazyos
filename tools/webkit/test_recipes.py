#!/usr/bin/env python3
"""The recipe table and the pins it is built from: every pinned library has a
recipe, the build order respects the dependencies, and the pins are the kind
the review asked for (hash-checked, no unexplained auto-generated archive)."""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import recipes  # noqa: E402
import wkpins  # noqa: E402


class TableTests(unittest.TestCase):
    def test_every_pinned_library_and_the_bundled_ones_have_a_recipe(self) -> None:
        for name in [*wkpins.BY_NAME, *wkpins.BUNDLED_BY_NAME]:
            self.assertIn(name, recipes.LIBS, name)

    def test_the_order_lists_every_recipe_once_and_the_proof_last(self) -> None:
        order = recipes.ORDER
        self.assertEqual(len(order), len(set(order)))
        self.assertEqual(order[-1], "depsproof")
        self.assertEqual(sorted(order[:-1]), sorted(recipes.LIBS))

    def test_a_library_comes_after_what_it_needs(self) -> None:
        position = {name: i for i, name in enumerate(recipes.ORDER)}
        for name, lib in recipes.LIBS.items():
            for need in lib.needs:
                self.assertIn(need, recipes.LIBS, f"{name} needs {need}")
                self.assertLess(position[need], position[name], f"{need} before {name}")

    def test_there_is_no_dependency_cycle(self) -> None:
        def reaches(name: str, seen: tuple[str, ...]) -> None:
            self.assertNotIn(name, seen, " -> ".join((*seen, name)))
            for need in recipes.LIBS[name].needs:
                reaches(need, (*seen, name))

        for name in recipes.LIBS:
            reaches(name, ())

    def test_only_the_bundled_libraries_build_their_own_tree(self) -> None:
        for name, lib in recipes.LIBS.items():
            self.assertEqual(lib.own_tree, name in wkpins.BUNDLED_BY_NAME, name)

    def test_a_recipe_without_a_pin_names_its_source_digest(self) -> None:
        for name, lib in recipes.LIBS.items():
            if name not in wkpins.BY_NAME:
                self.assertRegex(lib.extra.get("source_sha", ""), r"^[0-9a-f]{64}$", name)

    def test_the_cmake_options_build_static_libraries(self) -> None:
        import recipes_media
        import recipes_net
        import recipes_text
        tables = {"ZLIB": recipes_media.ZLIB, "BROTLI": recipes_media.BROTLI, "PNG": recipes_media.PNG,
                  "JPEG": recipes_media.JPEG, "WEBP": recipes_media.WEBP, "EXPAT": recipes_text.EXPAT,
                  "NGHTTP2": recipes_net.NGHTTP2, "CURL": recipes_net.CURL}
        for name, options in tables.items():
            shared = [key for key, value in options.items() if "SHARED" in key and value == "ON"]
            self.assertEqual(shared, [], name)

    def test_proof_links_only_libraries_that_have_recipes(self) -> None:
        import proof
        built = {lib for lib in recipes.LIBS}
        wanted = {"libcurl": "curl", "libnghttp2": "nghttp2", "freetype2": "freetype", "harfbuzz": "harfbuzz",
                  "fontconfig": "fontconfig", "expat": "expat", "libxml-2.0": "libxml2", "libwebp": "libwebp",
                  "libpng": "libpng", "libjpeg": "libjpeg-turbo", "libpsl": "libpsl", "sqlite3": "sqlite",
                  "icu-uc": "icu", "openssl": "openssl", "zlib": "zlib", "libbrotlidec": "brotli"}
        for module in proof.MODULES:
            if module in wanted:
                self.assertIn(wanted[module], built, module)


class PinTests(unittest.TestCase):
    def test_every_pin_is_a_sha256_and_an_https_or_known_url(self) -> None:
        for dep in wkpins.DEPS:
            self.assertRegex(dep.source.sha256, r"^[0-9a-f]{64}$", dep.name)
            self.assertRegex(dep.source.url, r"^https://", dep.name)
            self.assertTrue(dep.license and dep.why, dep.name)
            for mirror in dep.mirrors:
                self.assertRegex(mirror, r"^https://", dep.name)
                self.assertEqual(mirror.rsplit("/", 1)[1], dep.archive, dep.name)

    def test_archive_names_are_unique_in_the_cache(self) -> None:
        names = [dep.archive for dep in [*wkpins.DEPS, *wkpins.BUNDLED]]
        self.assertEqual(len(names), len(set(names)))

    def test_a_forge_generated_archive_is_explained_and_has_a_mirror(self) -> None:
        # GitHub does not promise the bytes of archive/refs/tags/*.tar.gz.
        for dep in wkpins.DEPS:
            if re.search(r"/archive/refs/tags/", dep.source.url):
                self.assertTrue(dep.notes and dep.mirrors, f"{dep.name}: explain it and mirror the bytes")

    def test_libpng_is_the_projects_release_tarball(self) -> None:
        dep = wkpins.BY_NAME["libpng"]
        self.assertIn("sourceforge.net/project/libpng/libpng16/", dep.source.url)
        self.assertTrue(dep.source.url.endswith(".tar.xz"))


if __name__ == "__main__":
    unittest.main()
