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

# The kernel tests' stand-in for DENY (kernel/src/tests/label_suite/tool.rs).
STAND_IN = """pub(super) const DENIED: [&str; 2] = [
    "os.lazy.keyd.v1",
    "os.lazy.process.label.spawn.v1",
];
pub(super) const UNLESS_NETWORK: [&str; 0] = [];
pub(super) const UNLESS_DEVELOP: [&str; 1] = ["os.lazy.process.label.spawn.v1"];
pub(super) const UNLESS_SHELL: [&str; 0] = [];
"""


class Parsing(unittest.TestCase):
    def test_deny_names_and_scopes_are_read_from_the_source(self):
        self.assertEqual(
            ci.deny_names(LIB),
            ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1"],
        )
        self.assertEqual(ci.kernel_scopes(LIB), ["os.lazy.process.label.spawn.v1"])
        self.assertEqual(ci.deny_names("nothing here"), [])

    def test_the_stand_in_names_are_read_from_a_pub_super_const(self):
        self.assertEqual(
            ci.stand_in_names(STAND_IN),
            ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1"],
        )
        self.assertEqual(ci.stand_in_names("nothing here"), [])

    def test_the_real_stand_in_is_found_and_equals_deny(self):
        stand_in = ci.stand_in_names(ci.STAND_IN.read_text(encoding="utf-8"))
        deny = ci.deny_names(ci.LIB.read_text(encoding="utf-8"))
        self.assertTrue(stand_in)
        self.assertEqual(sorted(stand_in), sorted(deny))

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

    def run_check(self, interfaces, reviewed, deny=None, scopes=None, stand_in=None):
        return ci.check(
            interfaces,
            self.DENY if deny is None else deny,
            self.SCOPES if scopes is None else scopes,
            reviewed,
            stand_in,
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

    def test_a_matching_stand_in_passes_in_any_order(self):
        interfaces = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1"]
        stand_in = ["os.lazy.process.label.spawn.v1", "os.lazy.keyd.v1"]
        self.assertEqual(self.run_check(interfaces, [], stand_in=stand_in), [])

    def test_a_deny_name_missing_from_the_stand_in_fails(self):
        interfaces = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1"]
        problems = self.run_check(interfaces, [], stand_in=["os.lazy.keyd.v1"])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("os.lazy.process.label.spawn.v1", problems[0])
        self.assertIn("kernel tests' DENIED", problems[0])
        self.assertIn("tool.rs", problems[0])

    def test_a_stand_in_name_that_is_not_denied_fails(self):
        interfaces = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1", "os.lazy.a.v1"]
        stand_in = list(self.DENY) + ["os.lazy.a.v1"]
        problems = self.run_check(interfaces, ["os.lazy.a.v1"], stand_in=stand_in)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("os.lazy.a.v1", problems[0])
        self.assertIn("not in DENY", problems[0])

    def test_a_repeating_or_empty_stand_in_fails(self):
        interfaces = ["os.lazy.keyd.v1", "os.lazy.process.label.spawn.v1"]
        repeated = self.run_check(interfaces, [], stand_in=self.DENY + ["os.lazy.keyd.v1"])
        self.assertTrue(any("repeats" in p for p in repeated), repeated)
        empty = self.run_check(interfaces, [], stand_in=[])
        self.assertTrue(any("no DENIED entries" in p for p in empty), empty)

    def test_an_empty_deny_list_fails(self):
        problems = self.run_check([], [], deny=[], scopes=[])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("no DENY entries", problems[0])


class Whens(unittest.TestCase):
    """The `when` column of the deny list against the kernel stand-in's lists
    (and a fourth `UnlessNetwork` entry: the hand copies must follow it)."""

    LIB4 = LIB.replace(
        "];\n\npub const KERNEL_SCOPES",
        """    Deny {
        interface: "os.lazy.net.extra.v1",
        // a comment between the fields is fine
        when: DenyWhen::UnlessNetwork,
        reason: "a fourth door to the network",
    },
];

pub const KERNEL_SCOPES""",
    )

    def test_the_whens_are_read_from_the_source(self):
        self.assertEqual(
            ci.deny_whens(LIB),
            [
                ("os.lazy.keyd.v1", "Always"),
                ("os.lazy.process.label.spawn.v1", "UnlessDevelop"),
            ],
        )
        self.assertEqual(
            ci.stand_in_whens(STAND_IN),
            {
                "UnlessNetwork": [],
                "UnlessDevelop": ["os.lazy.process.label.spawn.v1"],
                "UnlessShell": [],
            },
        )
        # A comment between `interface` and `when` is skipped.
        self.assertIn(("os.lazy.net.extra.v1", "UnlessNetwork"), ci.deny_whens(self.LIB4))

    def test_the_checked_in_columns_agree(self):
        deny = ci.deny_whens(ci.LIB.read_text(encoding="utf-8"))
        stand_in = ci.stand_in_whens(ci.STAND_IN.read_text(encoding="utf-8"))
        self.assertEqual(ci.check_whens(deny, stand_in), [])
        # Every entry that is lifted by something is in a list.
        lifted = {name for name, when in deny if when != "Always"}
        self.assertEqual(lifted, {n for names in stand_in.values() for n in names})

    def test_a_fourth_network_entry_must_reach_the_stand_in_list(self):
        deny = ci.deny_whens(self.LIB4)
        problems = ci.check_whens(deny, ci.stand_in_whens(STAND_IN))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("os.lazy.net.extra.v1", problems[0])
        self.assertIn("UNLESS_NETWORK", problems[0])
        fixed = STAND_IN.replace(
            "UNLESS_NETWORK: [&str; 0] = [];",
            'UNLESS_NETWORK: [&str; 1] = ["os.lazy.net.extra.v1"];',
        )
        self.assertEqual(ci.check_whens(deny, ci.stand_in_whens(fixed)), [])

    def test_an_entry_in_the_wrong_list_fails_both_ways(self):
        deny = ci.deny_whens(LIB)
        moved = STAND_IN.replace(
            "UNLESS_SHELL: [&str; 0] = [];",
            'UNLESS_SHELL: [&str; 1] = ["os.lazy.process.label.spawn.v1"];',
        )
        problems = ci.check_whens(deny, ci.stand_in_whens(moved))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("UNLESS_SHELL", problems[0])
        self.assertIn("not `DenyWhen::UnlessShell`", problems[0])
        dropped = STAND_IN.replace(
            'UNLESS_DEVELOP: [&str; 1] = ["os.lazy.process.label.spawn.v1"];',
            "UNLESS_DEVELOP: [&str; 0] = [];",
        )
        problems = ci.check_whens(deny, ci.stand_in_whens(dropped))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("UNLESS_DEVELOP", problems[0])

    def test_an_unknown_when_fails(self):
        problems = ci.check_whens(
            [("os.lazy.x.v1", "UnlessMoon")], ci.stand_in_whens(STAND_IN)
        )
        self.assertTrue(any("UnlessMoon" in p for p in problems), problems)

    def test_main_fails_on_a_drifting_when_list(self):
        declared = [
            {"interface": "os.lazy.keyd.v1"},
            {"interface": "os.lazy.process.label.spawn.v1"},
            {"interface": "os.lazy.net.extra.v1"},
        ]
        stand_in = STAND_IN.replace("[&str; 2]", "[&str; 3]").replace(
            '    "os.lazy.process.label.spawn.v1",\n];',
            '    "os.lazy.process.label.spawn.v1",\n    "os.lazy.net.extra.v1",\n];',
        )
        code, _, err = Main().run_main(declared, self.LIB4, "", stand_in=stand_in)
        self.assertEqual(code, 1)
        self.assertIn("UNLESS_NETWORK", err)


class Main(unittest.TestCase):
    def run_main(self, manifest, lib, reviewed, stand_in=STAND_IN):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            (root / "lib.rs").write_text(lib, encoding="utf-8")
            (root / "reviewed.txt").write_text(reviewed, encoding="utf-8")
            (root / "tool.rs").write_text(stand_in, encoding="utf-8")
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = ci.main(
                    [
                        "--manifest", str(root / "manifest.json"),
                        "--lib", str(root / "lib.rs"),
                        "--reviewed", str(root / "reviewed.txt"),
                        "--stand-in", str(root / "tool.rs"),
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

    def test_a_drifting_kernel_stand_in_fails_the_run(self):
        declared = [
            {"interface": "os.lazy.keyd.v1"},
            {"interface": "os.lazy.process.label.spawn.v1"},
        ]
        drifted = STAND_IN.replace('    "os.lazy.process.label.spawn.v1",\n];', "];")
        code, out, err = self.run_main(declared, LIB, "", stand_in=drifted)
        self.assertEqual(code, 1)
        self.assertIn("kernel tests' DENIED", err)
        self.assertIn("os.lazy.process.label.spawn.v1", err)

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
