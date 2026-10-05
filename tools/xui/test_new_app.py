#!/usr/bin/env python3
"""Tests of the app scaffolder (`tools/xui/new_app.py`).

They run it against a scratch copy of the files it edits, so the repository is
never touched, and check that every registry names the new app.

    python tools/xui/test_new_app.py
"""

from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import new_app  # noqa: E402

REGISTRIES = [
    "xui-app/Cargo.toml",
    "tools/xui/build.py",
    "build_support/xui_embed.rs",
    "build_support/core_packages.rs",
    "tools/xui/core_packages.py",
    "tools/run_demo.py",
    "tools/lazygui/catalog.py",
    "tools/screenshot/examples/core_apps.json",
    "xui-app/crates/app-icons/src/lib.rs",
]


class NewAppTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        for rel in REGISTRIES:
            (self.root / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(new_app.ROOT / rel, self.root / rel)
        self.app = new_app.App("notes", "Notes", "Quick notes", "accessories")

    def text(self, rel: str) -> str:
        return (self.root / rel).read_text(encoding="utf-8")

    def test_every_registry_names_the_new_app(self) -> None:
        touched = new_app.scaffold(self.root, self.app)
        self.assertEqual(len(touched), len(REGISTRIES) + 4)
        source = self.text("xui-app/src/bin/notes.rs")
        self.assertIn('launch::run("NOTES", "Notes"', source)
        self.assertIn("NOTES:UP:PASS", source)
        self.assertIn("NOTES:QUIT:PASS", source)
        self.assertIn('system_name = "os.lazy.notes"', self.text("xui-app/packages/notes/manifest.toml"))
        self.assertIn("NOTES:UP:PASS", self.text("tools/screenshot/examples/xui_notes.json"))
        expected = {
            "xui-app/Cargo.toml": 'name = "xui-notes"\npath = "src/bin/notes.rs"',
            "tools/xui/build.py": '    "xui-notes": "xui-notes.elf",\n}',
            "build_support/xui_embed.rs": '    "xui-notes.elf",\n];',
            "build_support/core_packages.rs": '        "notes",\n    ];',
            "tools/xui/core_packages.py": '    "notes": xui_app("xui-notes.elf", "notes"),\n}',
            "tools/run_demo.py": '"xui-notes.elf",\n)]',
            "tools/lazygui/catalog.py": '"settings", "devices", "notes"]',
            "tools/screenshot/examples/core_apps.json": "os.lazy.archiver os.lazy.notes",
            "xui-app/crates/app-icons/src/lib.rs": '"xui-app/packages/notes",',
        }
        for rel, needle in expected.items():
            self.assertIn(needle, self.text(rel).replace("\r\n", "\n"), rel)

    def test_a_second_run_refuses_and_changes_nothing(self) -> None:
        new_app.scaffold(self.root, self.app)
        before = {rel: self.text(rel) for rel in REGISTRIES}
        with self.assertRaises(new_app.ScaffoldError):
            new_app.scaffold(self.root, self.app)
        self.assertEqual(before, {rel: self.text(rel) for rel in REGISTRIES})

    def test_an_unexpected_registry_leaves_the_tree_untouched(self) -> None:
        (self.root / "tools/run_demo.py").write_text("# no list here\n", encoding="utf-8")
        before = {rel: self.text(rel) for rel in REGISTRIES}
        with self.assertRaises(new_app.ScaffoldError):
            new_app.scaffold(self.root, self.app)
        self.assertEqual(before, {rel: self.text(rel) for rel in REGISTRIES})
        self.assertFalse((self.root / "xui-app/src/bin/notes.rs").exists())

    def test_quotes_and_backslashes_stay_inside_their_literals(self) -> None:
        import ast
        import tomllib

        app = new_app.App("quoted", 'Say "hi"', "Reads C:\\notes", "accessories")
        new_app.scaffold(self.root, app)
        manifest = tomllib.loads(self.text("xui-app/packages/quoted/manifest.toml"))
        self.assertEqual(manifest["app"]["name"], 'Say "hi"')
        self.assertEqual(manifest["app"]["description"], "Reads C:\\notes")
        source = self.text("xui-app/src/bin/quoted.rs")
        self.assertIn('label("Say \\"hi\\"").title()', source)
        ast.parse(self.text("tools/lazygui/catalog.py"))

    def test_names_derive_from_the_short_id(self) -> None:
        self.assertEqual(self.app.marker, "NOTES")
        self.assertEqual(self.app.type_name, "Notes")
        self.assertEqual(self.app.elf, "xui-notes.elf")


if __name__ == "__main__":
    unittest.main()
