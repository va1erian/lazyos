"""Tests for owners.py: the package list and the join with the owner table."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import owners  # noqa: E402

PACKAGES = [
    {"name": "lazyos-init", "kind": "service", "activation": "reboot", "paths": ["/system/bin/init"]},
    {"name": "lazyos-docs", "kind": "data", "activation": "none", "producers": ["docs_embed"]},
    {"name": "lazyos-apps", "kind": "app", "activation": "next-launch", "paths": ["/system/packages/*"]},
    {"name": "lazyos-samples", "kind": "data", "activation": "none", "producers": ["samples_embed"]},
]


def check(table: str, seeds: list[str] | None = None):
    return owners.check(table, PACKAGES, seeds or [])


def load(text: str):
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "packages.toml"
        path.write_text(text, encoding="utf-8")
        return owners.load(path)


class RealListTests(unittest.TestCase):
    def test_the_checked_in_list_is_well_formed(self) -> None:
        _, _, problems = owners.load(owners.ROOT / "packaging" / "packages.toml")
        self.assertEqual(problems, [])


class CheckTests(unittest.TestCase):
    def test_a_path_match_and_a_producer_match_each_find_one_owner(self) -> None:
        counts, problems = check("/system/bin/init\tbuild.rs\n/docs/os/a.md\tdocs_embed\n")
        self.assertEqual(problems, [])
        self.assertEqual(counts, {"lazyos-init": 1, "lazyos-docs": 1})

    def test_an_unowned_file_is_reported_with_its_producer(self) -> None:
        _, problems = check("/system/bin/mystery\tbuild.rs\n")
        self.assertEqual(problems, ["unowned: /system/bin/mystery (placed by build.rs)"])

    def test_a_path_rule_beats_the_producer_default(self) -> None:
        # The sample package is placed by samples_embed but named by its path.
        counts, problems = check("/system/packages/os.lazy.paint.lzp\tsamples_embed\n")
        self.assertEqual(problems, [])
        self.assertEqual(counts, {"lazyos-apps": 1})

    def test_two_packages_claiming_a_path_is_ambiguous(self) -> None:
        packages = PACKAGES + [{"name": "lazyos-other", "paths": ["/system/bin/*"]}]
        _, problems = owners.check("/system/bin/init\tbuild.rs\n", packages, [])
        self.assertEqual(len(problems), 1)
        self.assertIn("ambiguous", problems[0])

    def test_seeds_have_no_package(self) -> None:
        counts, problems = check("/accounts/db\taccounts_seed\n", ["/accounts/db"])
        self.assertEqual(problems, [])
        self.assertEqual(counts, {"(seed)": 1})

    def test_a_malformed_line_is_reported(self) -> None:
        _, problems = check("/system/bin/init\n")
        self.assertEqual(len(problems), 1)


class LoadTests(unittest.TestCase):
    def test_bad_lists_are_refused(self) -> None:
        cases = {
            "name": '[[package]]\nname = "Bad Name"\nkind = "tool"\nactivation = "none"\npaths = ["/a"]\n',
            "kind": '[[package]]\nname = "a"\nkind = "gadget"\nactivation = "none"\npaths = ["/a"]\n',
            "activation": '[[package]]\nname = "a"\nkind = "tool"\nactivation = "soon"\npaths = ["/a"]\n',
            "relative path": '[[package]]\nname = "a"\nkind = "tool"\nactivation = "none"\npaths = ["a"]\n',
            "claims nothing": '[[package]]\nname = "a"\nkind = "tool"\nactivation = "none"\n',
            "duplicate": '[[package]]\nname = "a"\nkind = "tool"\nactivation = "none"\npaths = ["/a"]\n' * 2,
            "shared producer": (
                '[[package]]\nname = "a"\nkind = "tool"\nactivation = "none"\nproducers = ["x"]\n'
                '[[package]]\nname = "b"\nkind = "tool"\nactivation = "none"\nproducers = ["x"]\n'
            ),
        }
        for name, text in cases.items():
            with self.subTest(name):
                try:
                    _, _, problems = load(text)
                except Exception:  # a duplicate table key is a TOML error, also a refusal
                    problems = ["toml"]
                self.assertTrue(problems, name)

    def test_a_boot_volume_package_needs_no_paths(self) -> None:
        text = '[[package]]\nname = "k"\nkind = "kernel"\nactivation = "reboot"\nvolume = "boot"\n'
        self.assertEqual(load(text)[2], [])


if __name__ == "__main__":
    unittest.main()
