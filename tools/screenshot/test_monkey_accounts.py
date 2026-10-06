#!/usr/bin/env python3
"""Host tests for monkey_accounts.py and monkey_audit.py: ``python test_monkey_accounts.py``."""

import collections
import random
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import monkey_accounts as ma  # noqa: E402
import monkey_audit as au  # noqa: E402


class Profile(unittest.TestCase):
    def test_weights_cover_every_kind_and_follow_the_mix(self):
        rng = random.Random(1)
        seen = collections.Counter(ma.profile_kind(rng) for _ in range(20000))
        self.assertEqual(set(seen), set(ma.PROFILE))
        total = sum(ma.PROFILE.values())
        for kind, weight in ma.PROFILE.items():
            self.assertAlmostEqual(seen[kind] / 20000, weight / total, delta=0.02)

    def test_passwords_are_typeable_and_varied(self):
        rng = random.Random(2)
        pws = [ma.random_password(rng) for _ in range(500)]
        self.assertTrue(any(p == "" for p in pws))
        self.assertTrue(any(len(p) >= 100 for p in pws))
        self.assertTrue(all(c in ma.ALNUM + ma.SYMBOLS for p in pws for c in p))

    def test_seeded_actions_repeat(self):
        class Inner:
            qmp = None

            def __init__(self, seed):
                self.rng = random.Random(seed)

            def next_action(self):
                return {"a": "click"}

        def run(seed):
            m = ma.AccountMonkey(Inner(seed), lambda: None)
            return [m.next_action() for _ in range(100)]

        self.assertEqual(run(5), run(5))
        self.assertNotEqual(run(5), run(6))

    def test_replay_detection(self):
        self.assertTrue(ma.uses_accounts([{"a": "click"}, {"a": "password", "s": "x"}]))
        self.assertFalse(ma.uses_accounts([{"a": "click"}]))
        self.assertFalse(ma.uses_accounts(None))


class Probing(unittest.TestCase):
    def setUp(self):
        self.p = ma.Probe()
        for line in ["UI:RECT x=0 y=690 w=60 h=30 name=taskbar:start",
                     "UI:RECT x=0 y=400 w=200 h=24 name=menu:Settings",
                     "UI:RECT x=0 y=424 w=200 h=24 name=menu:Shut down",
                     "UI:RECT x=300 y=100 w=400 h=300 name=window:Settings",
                     "UI:RECT x=300 y=100 w=400 h=300 name=window:Calculator",
                     "UI:WIDGET x=10 y=20 w=80 h=20 name=accounts_tab window=Settings",
                     "UI:WIDGET x=10 y=60 w=80 h=20 name=wallpaper window=Settings",
                     "serial noise"]:
            self.p.feed(line)

    def test_pixels_resolve_and_missing_targets_are_none(self):
        self.assertEqual(self.p.pixel("taskbar:start"), (30, 705))
        self.assertEqual(self.p.pixel({"window": "Settings", "widget": "accounts_tab"}), (350, 130))
        self.assertIsNone(self.p.pixel("login:password"))

    def test_candidates_skip_power_rows_menus_and_boring_windows(self):
        cands = ma.goto_candidates(self.p)
        self.assertIn("window:Settings", cands)
        self.assertIn({"window": "Settings", "widget": "accounts_tab"}, cands)
        self.assertNotIn("window:Calculator", cands)
        self.assertFalse(any(isinstance(c, str) and (c.startswith("menu:") or "Shut" in c) for c in cands))

    def test_login_and_elevd_targets_are_used_when_printed(self):
        self.p.feed("UI:RECT x=1 y=1 w=10 h=10 name=login:password")
        self.p.feed("UI:RECT x=1 y=1 w=10 h=10 name=elevd:password")
        cands = ma.goto_candidates(self.p)
        self.assertIn("login:password", cands)
        self.assertIn("elevd:password", cands)

    def test_forget_menus_drops_stale_rows_only(self):
        self.p.forget_menus()
        self.assertIsNone(self.p.pixel("menu:Settings"))
        self.assertIsNotNone(self.p.pixel("taskbar:start"))

    def test_power_rows_are_bad_targets(self):
        self.assertTrue(ma.BAD_TARGET.search("menu:Shut down"))
        self.assertTrue(ma.BAD_TARGET.search("menu:Restart"))
        self.assertFalse(ma.BAD_TARGET.search("menu:Settings"))


class InvariantChecks(unittest.TestCase):
    def test_grant_without_password_is_a_finding(self):
        inv = ma.Invariants()
        out = inv.observe(["boot", "ELEVD:GRANT op=install uid=1000"])
        self.assertEqual(len(out), 1)
        self.assertTrue(out[0].startswith(ma.FINDING_PREFIX))

    def test_grant_after_admin_password_is_fine_once(self):
        inv = ma.Invariants()
        inv.credit()
        self.assertEqual(inv.observe(["ACCT:ELEVATE:GRANT uid=1000"]), [])
        self.assertEqual(len(inv.observe(["ACCT:ELEVATE:GRANT uid=1000"])), 1)

    def test_non_grant_markers_are_ignored_and_attacks_flagged(self):
        inv = ma.Invariants()
        self.assertEqual(inv.observe(["ELEVD:DENY wrong password", "ACCT:ATTACK:rm:BLOCKED:13"]), [])
        self.assertEqual(len(inv.observe(["ACCT:ATTACK:rm:SUCCEEDED:0"])), 1)

    def test_custom_pattern(self):
        inv = ma.Invariants(r"ELEVATED uid")
        self.assertEqual(len(inv.observe(["ELEVATED uid=0"])), 1)

    def test_observe_passes_lines_through_and_appends_findings(self):
        class Inner:
            qmp, rng = None, random.Random(0)

        m = ma.AccountMonkey(Inner(), lambda: None)
        out = m.observe(["UI:RECT x=1 y=2 w=3 h=4 name=taskbar:start", "ELEVD:GRANT"])
        self.assertEqual(out[:2], ["UI:RECT x=1 y=2 w=3 h=4 name=taskbar:start", "ELEVD:GRANT"])
        self.assertEqual(len(out), 3)
        self.assertEqual(m.probe.pixel("taskbar:start"), (2, 4))


TREE = "\n".join(["d 755 0 0 4096 - - /system/bin", "f 755 0 0 100 17 aaaa /system/bin/init",
                  "f 600 0 0 3 18 bbbb /conf/x", "d 700 1000 1000 1024 - - /home/user",
                  "f 644 1000 1000 2 19 eeee /home/user/a name"])


class Audit(unittest.TestCase):
    def test_parse_tree(self):
        nodes = au.parse_tree(TREE + "\nbroken line")
        self.assertEqual(len(nodes), 5)
        self.assertEqual(nodes["/conf/x"], ("f", "600", "0", "0", "3", "bbbb"))
        self.assertEqual(nodes["/system/bin"], ("d", "755", "0", "0", "0", "-"))
        self.assertIn("/home/user/a name", nodes)

    def test_parse_root_stat(self):
        self.assertEqual(au.parse_root_stat("/system", "755 0 0 4096\n"),
                         {"/system": ("d", "755", "0", "0", "0", "-")})
        self.assertEqual(au.parse_root_stat("/system", "mount: NotFound"), {})

    def test_mtime_alone_is_not_a_change(self):
        a = au.parse_tree("f 644 0 0 1 100 cccc /system/x")
        b = au.parse_tree("f 644 0 0 1 200 cccc /system/x")
        self.assertEqual(au.diff_trees(a, b), [])

    def test_diff_kinds(self):
        before = au.parse_tree(TREE)
        after = dict(before)
        del after["/conf/x"]
        after["/system/new"] = ("f", "644", "0", "0", "1", "cccc")
        after["/system/bin/init"] = ("f", "777", "1000", "0", "100", "dddd")
        changes = {c["path"]: c["change"] for c in au.diff_trees(before, after)}
        self.assertEqual(changes, {"/conf/x": "removed", "/system/new": "added",
                                   "/system/bin/init": "modified:mode,uid,hash"})
        self.assertEqual(au.diff_trees(before, before), [])

    def test_findings_respect_allowed_trees(self):
        prefixes = au.allowed_prefixes("user")
        changes = [{"path": p, "change": "added", "new": ()} for p in
                   ["/home/user/a", "/home/user", "/tmp/x", "/logs/service.log", "/transient/y",
                    "/home/admin/pwn", "/home/username/x", "/system/pwn", "/conf/k"]]
        found = au.findings_of(changes, prefixes)
        self.assertEqual([f.split()[-1] for f in found],
                         ["/home/admin/pwn", "/home/username/x", "/system/pwn", "/conf/k"])

    def test_directory_size_only_change_is_not_double_reported(self):
        change = {"path": "/system/bin", "change": "modified:size", "old": ("d",), "new": ("d",)}
        self.assertEqual(au.findings_of([change], []), [])
        change["old"] = ("f",)
        self.assertEqual(len(au.findings_of([change], [])), 1)

    def test_extra_allowed_prefix(self):
        c = [{"path": "/apps/os.lazy.x/1/bin", "change": "added", "new": ()}]
        self.assertEqual(len(au.findings_of(c, au.allowed_prefixes("user"))), 1)
        self.assertEqual(au.findings_of(c, au.allowed_prefixes("user", ["/apps"])), [])


if __name__ == "__main__":
    unittest.main()
