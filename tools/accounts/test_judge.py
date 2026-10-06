#!/usr/bin/env python3
"""The accounts judges fail when they should (and pass good logs).

    python tools/accounts/test_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import attack_judge  # noqa: E402
import audit  # noqa: E402
import boot_judge  # noqa: E402
from attack_judge import Expect, judge  # noqa: E402

TABLE = {
    "rm_system": Expect("blocked", "", ("/system/share/accounts",)),
    "confd_sys": Expect("xfail", "#623", ("/conf",)),
}


def log(**outcomes: str) -> str:
    return "".join(f"TERM:OUT:ACCT:ATTACK:{name}:{outcome}\n" for name, outcome in outcomes.items())


class AttackJudgeTest(unittest.TestCase):
    def test_a_blocked_gate_and_an_open_xfail_pass(self):
        verdict = judge(log(rm_system="BLOCKED:EACCES", confd_sys="SUCCEEDED:ok"), TABLE)
        self.assertEqual(verdict.failures, [])
        self.assertEqual(verdict.open_attacks, ["confd_sys"])
        self.assertTrue(any("xfail" in note for note in verdict.notes))

    def test_a_missing_marker_fails(self):
        failures = judge(log(rm_system="BLOCKED:EACCES"), TABLE).failures
        self.assertTrue(any("confd_sys" in f and "no ACCT:ATTACK" in f for f in failures))

    def test_an_unexpected_success_fails(self):
        failures = judge(log(rm_system="SUCCEEDED:ok", confd_sys="SUCCEEDED:ok"), TABLE).failures
        self.assertEqual(len(failures), 1)
        self.assertIn("rm_system", failures[0])

    def test_an_xpass_is_a_note_not_a_failure(self):
        verdict = judge(log(rm_system="BLOCKED:EACCES", confd_sys="BLOCKED:CONFD_DENIED"), TABLE)
        self.assertEqual(verdict.failures, [])
        self.assertTrue(any("XPASS" in note and "flip" in note for note in verdict.notes))
        self.assertEqual(verdict.open_attacks, [])

    def test_enoent_and_errors_are_inconclusive(self):
        failures = judge(log(rm_system="BLOCKED:ENOENT", confd_sys="ERROR:nopid"), TABLE).failures
        self.assertEqual(len(failures), 2)

    def test_an_unknown_scenario_fails(self):
        failures = judge(log(rm_system="BLOCKED:EACCES", confd_sys="SUCCEEDED:ok",
                             mystery="BLOCKED:EPERM"), TABLE).failures
        self.assertTrue(any("mystery" in f for f in failures))

    def test_the_shipped_table_is_all_xfail_with_an_issue_until_u0_lands(self):
        for name, expect in attack_judge.EXPECTATIONS.items():
            self.assertIn(expect.state, ("blocked", "xfail"), name)
            if expect.state == "xfail":
                self.assertTrue(expect.issue, f"{name}: an xfail needs its issue")


class BootJudgeTest(unittest.TestCase):
    STOP = "INIT:SHUTDOWN:BEGIN x\nINIT:SHUTDOWN:QUIESCED killed=0\npower: filesystems synced\n"
    UP = "TERM:UP:PASS\nTERM:OUT:ACCT:BOOT:OK\n"

    def test_good_logs_pass(self):
        self.assertEqual(boot_judge.judge_stop(self.STOP), [])
        self.assertEqual(boot_judge.judge_boot(self.UP, "verify"), [])
        self.assertEqual(boot_judge.judge_boot(
            self.UP + "ext2: / was not cleanly unmounted\n", "kill", after_hard_kill=True), [])

    def test_no_reboot_marker_fails(self):
        self.assertTrue(boot_judge.judge_boot("TERM:UP:PASS\n", "verify"))
        self.assertTrue(boot_judge.judge_boot("INIT:AUTOSTART:PASS\n", "verify"))

    def test_a_panic_fails(self):
        self.assertTrue(boot_judge.judge_boot(self.UP + "kernel panic: oops\n", "verify"))

    def test_an_unclean_volume_after_a_clean_stop_fails(self):
        self.assertTrue(boot_judge.judge_boot(
            self.UP + "ext2: / was not cleanly unmounted\n", "verify"))

    def test_a_hard_kill_that_left_the_volume_clean_means_it_was_not_killed(self):
        self.assertTrue(boot_judge.judge_boot(self.UP, "kill", after_hard_kill=True))

    def test_a_stop_without_the_sync_fails(self):
        self.assertTrue(boot_judge.judge_stop(self.STOP.replace("power: filesystems synced", "")))
        self.assertTrue(boot_judge.judge_stop(""))


NODE = "f 644 0 0 10 100 00000000000000aa {}"
LISTING = "\n".join([NODE.format("/system/bin/init"), "d 755 0 0 4096 - - /system",
                     NODE.format("/system/share/accounts/canary"),
                     NODE.format("/conf/store"), NODE.format("/home/user/notes"),
                     NODE.format("/logs/service.log")])


class AuditTest(unittest.TestCase):
    def setUp(self):
        self.before = audit.parse(LISTING)

    def changes(self, listing: str, excused=()):
        return audit.judge(audit.diff(self.before, audit.parse(listing)), list(excused))

    def test_identical_images_pass(self):
        self.assertEqual(self.changes(LISTING), ([], []))

    def test_the_users_home_and_journals_may_change(self):
        edited = LISTING.replace("00000000000000aa /home/user/notes", "00000000000000bb /home/user/notes") \
            .replace("100 00000000000000aa /logs", "200 00000000000000cc /logs")
        self.assertEqual(self.changes(edited), ([], []))

    def test_a_changed_system_file_fails(self):
        edited = LISTING.replace("00000000000000aa /system/bin/init", "00000000000000bb /system/bin/init")
        failures, _ = self.changes(edited)
        self.assertEqual(len(failures), 1)
        self.assertIn("/system/bin/init", failures[0])

    def test_a_removed_added_or_chmodded_path_fails(self):
        removed = LISTING.replace(NODE.format("/system/share/accounts/canary"), "")
        self.assertEqual(len(self.changes(removed)[0]), 1)
        added = LISTING + "\n" + NODE.format("/conf/planted")
        self.assertEqual(len(self.changes(added)[0]), 1)
        chmodded = LISTING.replace("f 644 0 0 10 100 00000000000000aa /conf/store",
                                   "f 666 0 0 10 100 00000000000000aa /conf/store")
        self.assertEqual(len(self.changes(chmodded)[0]), 1)

    def test_an_open_attack_excuses_only_its_paths(self):
        edited = LISTING.replace("00000000000000aa /conf/store", "00000000000000bb /conf/store") \
            .replace("00000000000000aa /system/bin/init", "00000000000000bb /system/bin/init")
        failures, notes = self.changes(edited, ["/conf"])
        self.assertEqual(len(failures), 1)
        self.assertIn("/system/bin/init", failures[0])
        self.assertEqual(len(notes), 1)

    def test_a_directory_mtime_is_not_a_change(self):
        edited = LISTING.replace("d 755 0 0 4096 - - /system", "d 755 0 0 8192 - - /system")
        self.assertEqual(self.changes(edited), ([], []))

    def test_prefixes_match_whole_segments(self):
        self.assertTrue(audit.under("/home/user/a", audit.ALLOWED))
        self.assertFalse(audit.under("/home/username/a", audit.ALLOWED))
        self.assertFalse(audit.under("/home/admin/a", audit.ALLOWED))


if __name__ == "__main__":
    unittest.main()
