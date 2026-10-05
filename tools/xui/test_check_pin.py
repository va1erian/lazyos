"""Unit tests for tools/xui/check_pin.py: python tools/xui/test_check_pin.py"""

from __future__ import annotations

import io
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_pin  # noqa: E402

NEW = "efd472229b563ea5d86186c6e1556d0f94492d27"
OLD = "48e504e52e0e5cbe20ffcd1cbeb05da4c6714828"


def manifest(rev: str, url: str = "https://github.com/va1erian/xui") -> str:
    return f"""
[package]
name = "app"
version = "0.1.0"

[dependencies]
xui-core = {{ git = "{url}", rev = "{rev}" }}
serde = "1"
"""


def lock(rev: str, name: str = "xui-core") -> str:
    return f"""
version = 4

[[package]]
name = "{name}"
version = "0.1.0"
source = "git+https://github.com/va1erian/xui?rev={rev}#{rev}"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""


class CheckPinTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())

    def write(self, path: str, text: str) -> None:
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text)

    def run_check(self, lagging=None):
        """The exit status and output of the script on the scratch tree."""
        out = io.StringIO()
        with mock.patch.object(check_pin, "LAGGING", lagging or {}), redirect_stdout(out):
            status = check_pin.main(["--root", str(self.root)])
        return status, out.getvalue()

    def test_one_revision_everywhere_passes(self):
        self.write("xui-app/Cargo.toml", manifest(NEW))
        self.write("xui-app/Cargo.lock", lock(NEW))
        self.write("doom/Cargo.toml", manifest(NEW))
        status, out = self.run_check()
        self.assertEqual(status, 0, out)
        self.assertIn(NEW, out)

    def test_a_manifest_on_another_revision_fails_and_names_it(self):
        self.write("xui-app/Cargo.toml", manifest(NEW))
        self.write("doom/Cargo.toml", manifest(OLD))
        status, out = self.run_check()
        self.assertEqual(status, 1)
        self.assertIn("disagree", out)
        self.assertIn("doom/Cargo.toml: dependencies.xui-core", out)

    def test_a_stale_lockfile_fails(self):
        self.write("xui-app/Cargo.toml", manifest(NEW))
        self.write("xui-app/Cargo.lock", lock(OLD))
        status, out = self.run_check()
        self.assertEqual(status, 1)
        self.assertIn("xui-app/Cargo.lock: package xui-core", out)

    def test_patch_entries_and_the_www_alias_count(self):
        self.write("xui-app/Cargo.toml", manifest(NEW))
        self.write(
            "rad/Cargo.toml",
            manifest(NEW)
            + f"""
[patch."https://github.com/va1erian/xui"]
xui-core = {{ git = "https://www.github.com/va1erian/xui", rev = "{OLD}" }}
""",
        )
        status, out = self.run_check()
        self.assertEqual(status, 1)
        self.assertIn('rad/Cargo.toml: patch.https://github.com/va1erian/xui.xui-core', out)

    def test_a_dependency_without_rev_fails(self):
        self.write("xui-app/Cargo.toml", manifest(NEW))
        self.write(
            "other/Cargo.toml",
            '[dependencies]\nxui-core = { git = "https://github.com/va1erian/xui", branch = "main" }\n',
        )
        status, out = self.run_check()
        self.assertEqual(status, 1)
        self.assertIn("has no `rev`", out)

    def test_other_git_dependencies_are_ignored(self):
        self.write("xui-app/Cargo.toml", manifest(NEW))
        self.write("rad/Cargo.toml", manifest(OLD, url="https://github.com/va1erian/lazyrad"))
        status, out = self.run_check()
        self.assertEqual(status, 0, out)

    def test_build_output_is_not_searched(self):
        self.write("xui-app/Cargo.toml", manifest(NEW))
        self.write("xui-app/target/debug/build/x/Cargo.toml", manifest(OLD))
        status, out = self.run_check()
        self.assertEqual(status, 0, out)

    def test_a_lagging_workspace_must_stay_at_its_revision(self):
        self.write("xui-app/Cargo.toml", manifest(NEW))
        self.write("rad/Cargo.toml", manifest(OLD))
        self.write("rad/Cargo.lock", lock(OLD))
        held = {"rad": (OLD, "waiting for a dependency")}
        status, out = self.run_check(held)
        self.assertEqual(status, 0, out)
        self.assertIn("rad held at", out)

        self.write("rad/Cargo.lock", lock("0" * 40))
        status, out = self.run_check(held)
        self.assertEqual(status, 1)
        self.assertIn("LAGGING holds rad", out)

    def test_a_lagging_workspace_that_caught_up_must_leave_the_list(self):
        self.write("xui-app/Cargo.toml", manifest(NEW))
        self.write("rad/Cargo.toml", manifest(NEW))
        status, out = self.run_check({"rad": (OLD, "waiting")})
        self.assertEqual(status, 1)
        self.assertIn("remove it from LAGGING", out)

    def test_the_repository_itself_is_consistent(self):
        out = io.StringIO()
        with redirect_stdout(out):
            status = check_pin.main([])
        self.assertEqual(status, 0, out.getvalue())


if __name__ == "__main__":
    unittest.main()
