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
import prompt_judge  # noqa: E402
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

    def test_a_forged_audit_line_fails_even_when_the_scenario_says_blocked(self):
        good = log(rm_system="BLOCKED:EACCES", confd_sys="SUCCEEDED:ok")
        # A real line quotes the text inside its summary: not a forgery.
        quoted = ('ELEVD:REQUEST op=conf.set uid=1000 label=0 session=1 admin=- '
                  'outcome=cancelled summary="Set the setting sys/x to \\"a\\nELEVD:REQUEST '
                  'op=account.admin uid=0 label=0 session=0 admin=forged\\""\n')
        self.assertEqual(judge(good + quoted, TABLE).failures, [])
        forged = ("x\nELEVD:REQUEST op=account.admin uid=0 label=0 session=0 admin=forged "
                  "outcome=granted summary=FORGED\n")
        failures = judge(good + forged, TABLE).failures
        self.assertTrue(any("forged an ELEVD:REQUEST" in f for f in failures))

    def test_the_shipped_table_gates_u0_and_tracks_the_rest(self):
        for name, expect in attack_judge.EXPECTATIONS.items():
            self.assertIn(expect.state, ("blocked", "xfail"), name)
            if expect.state == "xfail":
                self.assertTrue(expect.issue, f"{name}: an xfail needs its issue")
                self.assertNotEqual(expect.issue, attack_judge.U0, f"{name}: U0 landed")

    def test_autostart_must_open_as_the_session_user(self):
        installed = "TERM:OUT:ACCT:INSTALL:autostart_pkg:OK\n"
        as_user = ("INIT:AUTOSTART:SESSION session=2 uid=1000 apps=2\n"
                   "INIT:AUTOSTART:PASS app=org.acct.autoprobe session=2\n")
        marker = attack_judge.autostart_marker(installed, as_user)
        self.assertEqual(marker, "ACCT:ATTACK:autostart_root:BLOCKED:uid=1000")
        # The boot-time autostart before U0: no session, root.
        as_root = "INIT:AUTOSTART:PASS app=org.acct.autoprobe\n"
        self.assertIn(":SUCCEEDED:", attack_judge.autostart_marker(installed, as_root))
        as_admin = as_user.replace("uid=1000", "uid=0")
        self.assertIn(":SUCCEEDED:", attack_judge.autostart_marker(installed, as_admin))
        self.assertIn(":ERROR:notinstalled", attack_judge.autostart_marker("", as_user))
        self.assertIn(":ERROR:notopened", attack_judge.autostart_marker(installed, ""))
        self.assertIn("/apps", attack_judge.side_effects())


class BootJudgeTest(unittest.TestCase):
    STOP = "INIT:SHUTDOWN:BEGIN x\nINIT:SHUTDOWN:QUIESCED killed=0\npower: filesystems synced\n"
    UP = "LOGIN:OK:PASS user=user uid=1000 session=1 pid=9\nTERM:UP:PASS\nTERM:OUT:ACCT:BOOT:OK\n"

    def test_good_logs_pass(self):
        self.assertEqual(boot_judge.judge_stop(self.STOP), [])
        self.assertEqual(boot_judge.judge_boot(self.UP, "verify"), [])
        self.assertEqual(boot_judge.judge_boot(
            self.UP + "ext2: / was not cleanly unmounted\n", "kill", after_hard_kill=True), [])

    def test_no_reboot_marker_fails(self):
        self.assertTrue(boot_judge.judge_boot("TERM:UP:PASS\n", "verify"))
        self.assertTrue(boot_judge.judge_boot("INIT:AUTOSTART:PASS\n", "verify"))
        # Up and answering, but nobody logged in: not a login session.
        no_login = self.UP.replace("LOGIN:OK:PASS", "LOGIN:DENIED:PASS")
        self.assertTrue(boot_judge.judge_boot(no_login, "verify"))

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


def png(path: Path, width: int, height: int, paint) -> None:
    """Write an RGB PNG whose pixel (x, y) is `paint(x, y)`."""
    import struct
    import zlib
    rows = b"".join(b"\0" + b"".join(bytes(paint(x, y)) for x in range(width))
                    for y in range(height))

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
                     + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


class PromptJudgeTest(unittest.TestCase):
    """The trusted prompt (U2): the panel must survive a window opening over
    it, the typing must reach the prompt alone (an inputd flood included),
    and each scenario is judged from its own part of the log."""

    CMD = "TERM:CMD:sh /system/share/accounts/attack.sh {}\n"
    UP = "XUID:PROMPT:KEYS source=inputd\nXUID:PROMPT:UP uid=1000 label=0\n"
    #: The Counter's content rectangle, over the middle of a 600x300 screen.
    RECT = "UI:RECT x=100 y=60 w=300 h=160 name=window:xui counter\n"
    WINDOW = "XUIAPP:COUNTER:PASS\n"

    @staticmethod
    def done(tag: str, keys: int = 8) -> str:
        return (f"XUID:PROMPT:DONE outcome=cancelled keys={keys}\n"
                "ELEVD:REQUEST op=time.set uid=1000 label=0 session=1 admin=- "
                "outcome=cancelled\n"
                f"TERM:OUT:ACCT:PROMPT:{tag}:CANCELLED\n")

    def over(self, rect: str | None = None) -> str:
        return (self.CMD.format("prompt_over") + self.UP + (rect or self.RECT) + self.WINDOW
                + self.done("over", 1))

    def keys(self, extra: str = "", keys: int = 8) -> str:
        return self.CMD.format("prompt_keys") + self.UP + extra + self.done("keys", keys)

    def flood(self, body: str | None = None, full: int = 37) -> str:
        body = self.UP + self.done("flood", 9) if body is None else body
        report = "TERM:OUT:ACCT:PROMPT:flood:CANCELLED"
        return self.CMD.format("input_flood") + body.replace(report, f"{report}:full={full}")

    def shots(self, covered: bool) -> Path:
        import tempfile
        out = Path(tempfile.mkdtemp())
        x, y, w, h = prompt_judge.panel_rect(600, 300)
        inside = lambda px, py: x <= px < x + w and y <= py < y + h  # noqa: E731
        png(out / "shot_prompt_up.png", 600, 300,
            lambda px, py: (40, 110, 70) if inside(px, py) else (0, 0, 0))
        png(out / "shot_prompt_window.png", 600, 300,
            lambda px, py: (200, 200, 200) if covered and inside(px, py) and px < x + w // 2
            else ((40, 110, 70) if inside(px, py) else (9, 9, 9)))
        return out

    def test_every_scenario_blocked(self):
        log = self.over() + self.keys() + self.flood()
        markers = prompt_judge.markers(log, self.shots(covered=False))
        self.assertIn("prompt_over:BLOCKED:panel_same=1.000", markers)
        self.assertIn("prompt_keys:BLOCKED:keys=8", markers)
        self.assertIn("input_flood:BLOCKED:keys=9_full=37", markers)
        names = ("prompt_over", "prompt_keys", "input_flood")
        verdict = judge(markers, {name: Expect("blocked") for name in names})
        self.assertEqual(verdict.failures, [])

    def test_a_window_drawn_over_the_panel_succeeds(self):
        self.assertIn(":SUCCEEDED:", prompt_judge.over_marker(self.over(), self.shots(True)))

    def test_a_window_away_from_the_panel_proves_nothing(self):
        far = "UI:RECT x=0 y=0 w=60 h=40 name=window:xui counter\n"
        marker = prompt_judge.over_marker(self.over(far), self.shots(covered=False))
        self.assertIn(":ERROR:nooverlap", marker)
        other = self.RECT.replace("xui counter", "Terminal")
        marker = prompt_judge.over_marker(self.over(other), self.shots(covered=False))
        self.assertIn(":ERROR:norect", marker)

    def test_a_window_that_never_opened_during_the_prompt_is_inconclusive(self):
        shots = self.shots(covered=False)
        no_window = self.over().replace(self.WINDOW, "")
        self.assertIn(":ERROR:nowindow", prompt_judge.over_marker(no_window, shots))
        late = no_window + self.WINDOW
        self.assertIn(":ERROR:nowindow", prompt_judge.over_marker(late, shots))
        self.assertIn(":ERROR:noprompt", prompt_judge.over_marker("", shots))

    def test_keys_that_reached_a_client_succeed(self):
        self.assertIn(":SUCCEEDED:", prompt_judge.keys_marker(self.keys("TERM:CMD:inject\n")))
        self.assertIn(":ERROR:", prompt_judge.keys_marker(self.keys(keys=7)))
        approved = self.keys().replace("outcome=cancelled keys", "outcome=approved keys")
        self.assertIn(":ERROR:", prompt_judge.keys_marker(approved))
        unheard = self.keys().replace("ACCT:PROMPT:keys:CANCELLED", "ACCT:PROMPT:keys:OTHER:x")
        self.assertIn(":ERROR:", prompt_judge.keys_marker(unheard))
        kernel = self.keys().replace("source=inputd", "source=kernel")
        self.assertIn(":ERROR:source=kernel", prompt_judge.keys_marker(kernel))

    def test_scenarios_never_borrow_each_others_lines(self):
        # Only prompt_over ran: prompt_keys has no prompt of its own.
        self.assertIn(":ERROR:", prompt_judge.keys_marker(self.over()))
        # A leak in the keys scenario is not the flood scenario's.
        log = self.keys("TERM:CMD:inject\n") + self.flood()
        self.assertIn("input_flood:BLOCKED", prompt_judge.flood_marker(log))

    def test_the_flood_must_be_real_and_keys_must_not_leak(self):
        self.assertIn(":ERROR:noflood", prompt_judge.flood_marker(self.flood(full=0)))
        self.assertIn(":ERROR:noreport", prompt_judge.flood_marker(self.CMD.format("input_flood")))
        leaked = self.flood(self.UP + "TERM:CMD:flooded\n" + self.done("flood", 9))
        self.assertIn(":SUCCEEDED:", prompt_judge.flood_marker(leaked))

    def test_a_refused_prompt_under_the_flood_is_blocked(self):
        refused = ("XUID:PROMPT:REFUSED reason=focus:Some(-11)\n"
                   "ELEVD:REQUEST op=time.set uid=1000 label=0 session=1 admin=- outcome=nokeys\n"
                   "TERM:CMD:flooded\nTERM:OUT:ACCT:PROMPT:flood:NOKEYS:full=12\n")
        marker = prompt_judge.flood_marker(self.CMD.format("input_flood") + refused)
        self.assertIn(":BLOCKED:refused", marker)
        unreported = refused.replace("outcome=nokeys", "outcome=refused")
        marker = prompt_judge.flood_marker(self.CMD.format("input_flood") + unreported)
        self.assertIn(":ERROR:", marker)


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
