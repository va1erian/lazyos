#!/usr/bin/env python3
"""webd's package manifest keeps its children narrow (review of 2026-10-22, finding 4).

The network process writes the profiles' folder and the Downloads folder, not the app's whole data folder:
`$HOME/.apps/os.lazy.webd/rules/` holds the compiled network-lock rule store the light views' no-network
guarantee rests on (WebKit compiles rule lists in the UI process), and a compromised network process must
not be able to rewrite it. The manifest is also checked by the shared rules (`tools/pkg/pkgmanifest.py`),
so a child rule wider than the app's is refused there.
"""

from __future__ import annotations

import sys
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "pkg"))

APP_FOLDER = "$HOME/.apps/os.lazy.webd"


def manifest() -> dict:
    path = ROOT / "xui-app" / "packages" / "webd" / "manifest.toml"
    return tomllib.loads(path.read_text(encoding="utf-8"))


def writes(rules: list[str]) -> list[str]:
    return [rule.removeprefix("write:") for rule in rules if rule.startswith("write:")]


class ManifestTest(unittest.TestCase):
    def test_the_network_process_cannot_write_the_rule_store(self) -> None:
        child = manifest()["processes"]["networkprocess"]["files"]
        self.assertEqual(sorted(writes(child)), sorted([f"{APP_FOLDER}/profiles", "$HOME/Downloads"]))
        for path in writes(child):
            self.assertNotEqual(path.rstrip("/"), APP_FOLDER, "the whole app folder includes rules/")
            self.assertFalse(f"{APP_FOLDER}/rules".startswith(path.rstrip("/") + "/"),
                             f"{path} covers the rule store")

    def test_the_page_process_writes_nothing(self) -> None:
        self.assertEqual(writes(manifest()["processes"]["webprocess"]["files"]), [])

    def test_every_child_rule_is_covered_by_the_apps(self) -> None:
        app = manifest()["permissions"]["files"]
        for name, table in manifest()["processes"].items():
            for rule in table["files"]:
                verb, path = rule.split(":", 1)
                covered = any(
                    other.split(":", 1)[0] == verb
                    and (path == other.split(":", 1)[1]
                         or path.startswith(other.split(":", 1)[1].rstrip("/") + "/"))
                    for other in app
                )
                self.assertTrue(covered, f"{name}: {rule} is not covered by the app's files")


if __name__ == "__main__":
    unittest.main()
