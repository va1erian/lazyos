#!/usr/bin/env python3
"""Self-test of tools/devtool/check_interfaces.py: it passes on the checked-in
files and fails, with the advice, when an interface is unclassified."""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import check_interfaces as ci  # noqa: E402

LIB = """\
pub const DENY: &[Deny] = &[
    Deny {
        interface: "os.lazy.keyd.v1",
        when: DenyWhen::Always,
        reason: "keys and secrets",
    },
    Deny {
        interface: "os.lazy.process.label.spawn.v1",
        when: DenyWhen::UnlessDevelop,
        reason: "running a child under another label",
    },
];

pub const KERNEL_SCOPES: &[&str] = &["os.lazy.process.label.spawn.v1"];
"""


class Parsing(unittest.TestCase):
    def test_deny_names_and_scopes_are_read_from_the_source(self):
        self.assertEqual(
            ci.deny_names(LIB),
            ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1"],
        )
        self.assertEqual(ci.kernel_scopes(LIB), ["os.lazy.process.label.spawn.v1"])
        self.assertEqual(ci.deny_names("nothing here"), [])

    def test_the_real_constants_are_found(self):
        source = ci.LIB.read_text(encoding="utf-8")
        self.assertIn("os.lazy.keyd.v1", ci.deny_names(source))
        self.assertTrue(ci.kernel_scopes(source))

    def test_the_reviewed_list_ignores_comments_and_blanks(self):
        text = "# header\n\nos.lazy.a.v1\n  os.lazy.b.v1  \n"
        self.assertEqual(ci.reviewed_names(text), ["os.lazy.a.v1", "os.lazy.b.v1"])

    def test_the_manifest_is_read_in_file_order(self):
        text = json.dumps([{"interface": "b.v1"}, {"interface": "a.v1"}])
        self.assertEqual(ci.manifest_interfaces(text), ["b.v1", "a.v1"])


class Check(unittest.TestCase):
    DENY = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1"]
    SCOPES = ["os.lazy.process.label.spawn.v1"]

    def run_check(self, interfaces, reviewed, deny=None, scopes=None):
        return ci.check(
            interfaces,
            self.DENY if deny is None else deny,
            self.SCOPES if scopes is None else scopes,
            reviewed,
        )

    def test_a_fully_classified_set_passes(self):
        interfaces = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1", "os.lazy.a.v1"]
        self.assertEqual(self.run_check(interfaces, ["os.lazy.a.v1"]), [])

    def test_a_new_interface_fails_with_the_advice(self):
        interfaces = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1", "os.lazy.new.v1"]
        problems = self.run_check(interfaces, [])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("os.lazy.new.v1", problems[0])
        self.assertIn("DENY", problems[0])
        self.assertIn("reviewed_interfaces.txt", problems[0])
        self.assertIn("decide whether a developer tool may call it", problems[0])

    def test_a_name_on_both_lists_fails(self):
        interfaces = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1"]
        problems = self.run_check(interfaces, ["os.lazy.keyd.v1"])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("DENY and in the reviewed list", problems[0])

    def test_a_stale_reviewed_line_fails(self):
        interfaces = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1"]
        problems = self.run_check(interfaces, ["os.lazy.gone.v1"])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("does not declare it", problems[0])

    def test_a_deny_name_nothing_declares_fails(self):
        problems = self.run_check(["os.lazy.keyd.v1"], [], scopes=[])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("os.lazy.process.label.spawn.v1", problems[0])
        self.assertIn("KERNEL_SCOPES", problems[0])
        # Listed as a kernel scope, it needs no declaration.
        self.assertEqual(self.run_check(["os.lazy.keyd.v1"], []), [])

    def test_an_unsorted_or_repeating_list_fails(self):
        interfaces = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1", "os.lazy.a.v1", "os.lazy.b.v1"]
        unsorted = self.run_check(interfaces, ["os.lazy.b.v1", "os.lazy.a.v1"])
        self.assertEqual(len(unsorted), 1, unsorted)
        self.assertIn("not sorted", unsorted[0])
        repeated = self.run_check(interfaces, ["os.lazy.a.v1", "os.lazy.a.v1", "os.lazy.b.v1"])
        self.assertTrue(any("repeats" in p for p in repeated), repeated)

    def test_an_empty_deny_list_fails(self):
        problems = self.run_check([], [], deny=[], scopes=[])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("no DENY entries", problems[0])


class Main(unittest.TestCase):
    def run_main(self, manifest, lib, reviewed):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            (root / "lib.rs").write_text(lib, encoding="utf-8")
            (root / "reviewed.txt").write_text(reviewed, encoding="utf-8")
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = ci.main(
                    [
                        "--manifest", str(root / "manifest.json"),
                        "--lib", str(root / "lib.rs"),
                        "--reviewed", str(root / "reviewed.txt"),
                    ]
                )
        return code, out.getvalue(), err.getvalue()

    def test_exit_codes_and_messages(self):
        declared = [
            {"interface": "os.lazy.keyd.v1"},
            {"interface": "os.lazy.process.label.spawn.v1"},
            {"interface": "os.lazy.a.v1"},
        ]
        code, out, err = self.run_main(declared, LIB, "os.lazy.a.v1\n")
        self.assertEqual((code, err), (0, ""))
        self.assertIn("PASS", out)
        declared.append({"interface": "os.lazy.new.v1"})
        code, out, err = self.run_main(declared, LIB, "os.lazy.a.v1\n")
        self.assertEqual(code, 1)
        self.assertIn("os.lazy.new.v1", err)
        self.assertEqual(out, "")

    def test_the_checked_in_files_pass(self):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ci.main([])
        self.assertEqual((code, err.getvalue()), (0, ""))

    def test_the_reviewed_file_is_sorted_and_free_of_denied_names(self):
        names = ci.reviewed_names(ci.REVIEWED.read_text(encoding="utf-8"))
        self.assertEqual(names, sorted(names))
        denied = ci.deny_names(ci.LIB.read_text(encoding="utf-8"))
        self.assertFalse(set(names) & set(denied))


if __name__ == "__main__":
    unittest.main()
