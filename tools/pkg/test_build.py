#!/usr/bin/env python3
"""Unit tests for the package builder: it must fail when it should.

A valid tree builds and reopens with `zipfile`; a missing icon, a bad
`system_name`, and an unknown top-level directory each fail with a clear
message. The builder enforces the same rules as the Rust reader in
`libs/lazypkg`, so these cases mirror its tests.
"""

from __future__ import annotations

import sys
import tempfile
import tomllib
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build  # noqa: E402

MANIFEST = """\
[app]
name = "Demo"
system_name = "org.lazy.demo"
author = "Tester"
version = "1.0.0"

[entry]
binary = "bin/app.elf"
"""

PNG = b"\x89PNG\r\n\x1a\n" + b"\x00\x00\x00\x0dIHDR"


def make_tree(root: Path, system_name: str = "org.lazy.demo", with_icon: bool = True) -> None:
    root.mkdir(parents=True, exist_ok=True)
    manifest = MANIFEST.replace("org.lazy.demo", system_name)
    (root / "manifest.toml").write_text(manifest, encoding="utf-8")
    (root / "bin").mkdir(exist_ok=True)
    (root / "bin" / "app.elf").write_bytes(b"ELF fake binary")
    (root / "icons").mkdir(exist_ok=True)
    for size in (16, 32, 128):
        if size == 32 and not with_icon:
            continue
        (root / "icons" / f"app-{size}.png").write_bytes(PNG)


class BuildTests(unittest.TestCase):
    def test_a_valid_tree_builds_and_reopens(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "src"
            out = Path(tmp) / "dist"
            make_tree(root)
            archive = build.build(root, out)
            self.assertEqual(archive.name, "org.lazy.demo-1.0.0.lzp")
            with zipfile.ZipFile(archive) as zf:
                names = set(zf.namelist())
                self.assertIn("manifest.toml", names)
                self.assertIn("bin/app.elf", names)
                self.assertEqual(
                    zf.getinfo("icons/app-16.png").compress_type, zipfile.ZIP_STORED
                )
                self.assertEqual(
                    zf.getinfo("manifest.toml").compress_type, zipfile.ZIP_DEFLATED
                )
                self.assertTrue(zf.read("icons/app-128.png").startswith(PNG))

    def test_a_missing_icon_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "src"
            make_tree(root, with_icon=False)
            with self.assertRaises(build.BuildError) as caught:
                build.build(root, Path(tmp) / "dist")
            self.assertIn("icons/app-32.png", str(caught.exception))

    def test_a_bad_system_name_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "src"
            make_tree(root, system_name="Org.Lazy.Demo")
            with self.assertRaises(build.BuildError) as caught:
                build.build(root, Path(tmp) / "dist")
            self.assertIn("system_name", str(caught.exception))

    def test_an_unknown_top_level_directory_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "src"
            make_tree(root)
            (root / "extra").mkdir()
            (root / "extra" / "foo.txt").write_text("x", encoding="utf-8")
            with self.assertRaises(build.BuildError) as caught:
                build.build(root, Path(tmp) / "dist")
            self.assertIn("extra/foo.txt", str(caught.exception))

    def test_an_overlong_entry_name_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "tree"
            make_tree(root)
            deep = root / "resources" / ("a" * 130) / ("b" * 130)
            deep.mkdir(parents=True)
            (deep / "x").write_bytes(b"x")
            with self.assertRaises(build.BuildError) as raised:
                build.build(root, Path(tmp))
            self.assertIn("longer than 255 bytes", str(raised.exception))

    def test_forbidden_entry_name_characters_are_rejected(self):
        # Backslashes and control characters cannot be exercised through real
        # files on every host filesystem, so the name check is tested directly.
        self.assertEqual(build.entry_name_problem("resources/a\\b"), "contains a backslash")
        self.assertEqual(build.entry_name_problem("resources/a\tb"), "contains a control character")
        self.assertEqual(build.entry_name_problem("resources/../x"), "has an empty, `.` or `..` path component")
        self.assertEqual(build.entry_name_problem("C:/x"), "has a drive letter")
        self.assertEqual(build.entry_name_problem("/etc/passwd"), "is absolute")
        self.assertIsNone(build.entry_name_problem("resources/ok.txt"))
        problems = build.validate_layout({"manifest.toml": None, "resources/a\tb": None})
        self.assertTrue(any("control character" in p for p in problems), problems)

    def test_manifest_size_boundary(self):
        for padding, ok in [(0, True), (1, False)]:
            with self.subTest(over=padding), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp) / "tree"
                make_tree(root)
                path = root / "manifest.toml"
                body = path.read_bytes()
                fill = build.MAX_MANIFEST - len(body) + padding
                path.write_bytes(body + b"#" + b"x" * (fill - 2) + b"\n")
                self.assertEqual(len(path.read_bytes()), build.MAX_MANIFEST + padding)
                if ok:
                    build.build(root, Path(tmp))
                else:
                    with self.assertRaises(build.BuildError) as raised:
                        build.build(root, Path(tmp))
                    self.assertIn("larger than", str(raised.exception))

    def test_wrong_field_types_are_reported_not_raised(self):
        manifest = MANIFEST + '\n[[mime]]\ntype = 123\nverbs = ["open"]\n\n[permissions]\ninterfaces = 7\ntopics = "x"\n'
        problems = build.validate_manifest(tomllib.loads(manifest))
        self.assertTrue(any("mime[0].type" in p for p in problems), problems)
        self.assertTrue(any("permissions.interfaces must be an array" in p for p in problems), problems)
        self.assertTrue(any("permissions.topics must be an array" in p for p in problems), problems)

    def test_a_missing_referenced_binary_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "src"
            make_tree(root)
            (root / "bin" / "app.elf").unlink()
            with self.assertRaises(build.BuildError) as caught:
                build.build(root, Path(tmp) / "dist")
            self.assertIn("bin/app.elf", str(caught.exception))

    def test_every_problem_is_reported(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "src"
            make_tree(root, system_name="Bad", with_icon=False)
            with self.assertRaises(build.BuildError) as caught:
                build.build(root, Path(tmp) / "dist")
            message = str(caught.exception)
            self.assertIn("system_name", message)
            self.assertIn("icons/app-32.png", message)


if __name__ == "__main__":
    unittest.main()
