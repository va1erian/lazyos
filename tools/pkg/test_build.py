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
import pkgmanifest  # noqa: E402

# Shared with `libs/lazypkg/tests/cases.rs`; the file documents its format.
CASES = Path(__file__).resolve().parents[2] / "libs" / "lazypkg" / "tests" / "cases" / "manifest.toml"

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

    def test_building_the_same_tree_twice_gives_the_same_archive(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "src"
            make_tree(root)
            first = build.build(root, Path(tmp) / "a").read_bytes()
            second = build.build(root, Path(tmp) / "b").read_bytes()
            self.assertEqual(first, second)

    def test_the_counter_sample_builds_with_a_stand_in_binary(self):
        import build_samples  # noqa: E402

        with tempfile.TemporaryDirectory() as tmp:
            xui = Path(tmp) / "xui"
            xui.mkdir()
            (xui / "xui-counter.elf").write_bytes(b"\x7fELF stand-in")
            out = Path(tmp) / "pkg"
            archive = build_samples.build_sample("counter", xui, out)
            self.assertEqual(archive.name, "pkgdemo.lzp")
            with zipfile.ZipFile(archive) as zf:
                names = set(zf.namelist())
                manifest = tomllib.loads(zf.read("manifest.toml").decode("utf-8"))
            self.assertEqual(manifest["app"]["system_name"], "org.lazy.counter")
            self.assertEqual(manifest["entry"]["abi"], "linux")
            self.assertEqual(
                manifest["permissions"]["interfaces"],
                ["os.lazy.display.v1", "os.lazy.input.v1"],
            )
            for icon in ("icons/app-16.png", "icons/app-32.png", "icons/app-128.png"):
                self.assertIn(icon, names)
            self.assertIn("bin/counter.elf", names)

    def test_a_missing_sample_binary_is_skipped_not_an_error(self):
        import build_samples  # noqa: E402

        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(build_samples.build_sample("counter", Path(tmp), Path(tmp) / "out"))

    def test_entry_abi_is_native_or_linux(self):
        for abi, ok in [("native", True), ("linux", True), ("windows", False)]:
            with self.subTest(abi=abi):
                manifest = MANIFEST + f'abi = "{abi}"\n'
                problems = build.validate_manifest(tomllib.loads(manifest))
                self.assertEqual(problems == [], ok, problems)
                if not ok:
                    self.assertTrue(any("entry.abi" in p for p in problems), problems)

    def test_every_problem_is_reported(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "src"
            make_tree(root, system_name="Bad", with_icon=False)
            with self.assertRaises(build.BuildError) as caught:
                build.build(root, Path(tmp) / "dist")
            message = str(caught.exception)
            self.assertIn("system_name", message)
            self.assertIn("icons/app-32.png", message)


def _case_manifest(case):
    """The template at the top of the shared cases file."""
    return (
        '[app]\nname = "Demo"\nsystem_name = "org.lazy.demo"\nauthor = "Tester"\n'
        f'version = "{case.get("version", "1.0.0")}"\n{case.get("app", "")}\n'
        f'[entry]\nbinary = "bin/app.elf"\n{case.get("entry", "")}\n'
        f'[permissions]\n{case.get("permissions", "")}\n'
    )


class SharedCaseTests(unittest.TestCase):
    """The cases `libs/lazypkg` runs too: the validators must agree."""

    @classmethod
    def setUpClass(cls):
        cls.cases = tomllib.loads(CASES.read_text(encoding="utf-8"))

    def test_manifest_cases(self):
        self.assertGreaterEqual(len(self.cases["manifest"]), 30, "the cases file lost cases")
        for case in self.cases["manifest"]:
            with self.subTest(case=case["name"]):
                manifest = tomllib.loads(_case_manifest(case))
                problems = build.validate_manifest(manifest)
                if case["valid"]:
                    self.assertEqual(problems, [])
                    app, entry = manifest["app"], manifest["entry"]
                    if "category" in case:
                        self.assertEqual(app.get("category", pkgmanifest.DEFAULT_CATEGORY), case["category"])
                    if "autostart" in case:
                        self.assertEqual(entry.get("autostart", False), case["autostart"])
                    if "develop" in case:
                        permissions = manifest.get("permissions", {})
                        self.assertEqual(permissions.get("develop", False), case["develop"])
                else:
                    self.assertTrue(any(case["error"] in p for p in problems), problems)

    def test_version_cases(self):
        versions = self.cases["versions"]
        for text in versions["valid"]:
            self.assertEqual(str(pkgmanifest.Version(text)), text)
        for case in versions["invalid"]:
            with self.subTest(version=case["text"]):
                self.assertEqual(pkgmanifest.version_problem(case["text"]), case["reason"])
        for chain in versions["ascending"]:
            parsed = [pkgmanifest.Version(text) for text in chain]
            for index, lower in enumerate(parsed):
                for higher in parsed[index + 1:]:
                    self.assertLess(lower, higher)
                    self.assertGreater(higher, lower)
                    self.assertNotEqual(lower, higher)
        for left, right in versions["equal"]:
            self.assertEqual(pkgmanifest.Version(left), pkgmanifest.Version(right))
            self.assertEqual(hash(pkgmanifest.Version(left)), hash(pkgmanifest.Version(right)))

    def test_the_absolute_home_switch_is_on_on_both_sides(self):
        # One line in each validator (F5 cleanup, issue #509).
        self.assertTrue(pkgmanifest.REJECT_ABSOLUTE_HOME)
        rust = (CASES.parents[2] / "src" / "files.rs").read_text(encoding="utf-8")
        self.assertIn("REJECT_ABSOLUTE_HOME: bool = true;", rust)
        for rule in ("read:/home/*/x", "write:/home/*/.apps/org.lazy.demo"):
            problem = pkgmanifest.file_rule_problem(rule)
            self.assertIsNotNone(problem, rule)
            self.assertIn("write it as $HOME/", problem)
        self.assertIsNone(pkgmanifest.file_rule_problem("write:$HOME/.apps/org.lazy.demo"))
        self.assertTrue(pkgmanifest.is_absolute_home("/home/*/x"))
        self.assertFalse(pkgmanifest.is_absolute_home("/homework"))

    def test_a_pre_release_package_builds_under_its_version(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "src"
            make_tree(root)
            path = root / "manifest.toml"
            text = path.read_text(encoding="utf-8").replace('"1.0.0"', '"1.1.0-rc1"')
            path.write_text(text + 'autostart = true\n', encoding="utf-8")
            archive = build.build(root, Path(tmp) / "dist")
            self.assertEqual(archive.name, "org.lazy.demo-1.1.0-rc1.lzp")


if __name__ == "__main__":
    unittest.main()
