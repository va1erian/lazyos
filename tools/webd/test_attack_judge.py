#!/usr/bin/env python3
"""The malicious-client attack judge (`attackjudge.py`) fails when it should: a clean run passes, and
each kind of breach, or a refusal that is not shown to be the kernel's, is caught."""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import attackjudge as aj  # noqa: E402

LABEL = aj.LABEL
NET = aj.interface_id("os.lazy.webd.net.v1")
BROWSER = aj.interface_id("os.lazy.webd.browser.v1")
VIEW = aj.interface_id("os.lazy.webd.view.v1")


def clean() -> str:
    """A serial log of a run where every probe did what it must (as the real run of 2026-10-22)."""
    return "\n".join(
        [
            "WEBD:UP:PASS",
            "WEBDATTACK:UP:PASS",
            f"LABEL:DENY label={LABEL} iface=0x{NET:016x} method=1",
            f"LABEL:DENY label={LABEL} iface=0x{BROWSER:016x} method=1",
            "WEBDATTACK:PROBE:scope_net:REFUSED:13",
            "WEBDATTACK:PROBE:scope_browser:REFUSED:13",
            "WEBDATTACK:PROBE:view_ok:OK",
            "WEBDATTACK:PROBE:flood_alive:OK",
            f"WEBDATTACK:PROBE:view_cap:REFUSED:{aj.ENOSPC}:{aj.VIEWS_PER_CLIENT}",
            "WEBDATTACK:PROBE:alive_after:OK",
            "WEBDATTACK:DONE:PASS",
            "WEBDATTACK:QUIT:PASS",
            "",
        ]
    )


class JudgeTests(unittest.TestCase):
    def assert_pass(self, serial: str) -> None:
        problems, _ = aj.judge(serial)
        self.assertEqual(problems, [], f"unexpected problems: {problems}")

    def assert_fail(self, serial: str, needle: str) -> None:
        problems, _ = aj.judge(serial)
        self.assertTrue(problems, "expected a problem, found none")
        self.assertTrue(any(needle in p for p in problems), f"{needle!r} not in {problems}")

    def test_a_clean_run_passes(self) -> None:
        self.assert_pass(clean())

    def test_the_saved_real_run_passes_when_there_is_one(self) -> None:
        saved = ROOT / "shots" / "webd-attack" / "serial.log"
        if not saved.is_file():
            self.skipTest("no saved run")
        self.assertEqual(aj.judge(saved.read_text(errors="replace"))[0], [])

    def test_a_scope_allowed_is_a_breach(self) -> None:
        self.assert_fail(clean().replace("scope_net:REFUSED:13", "scope_net:OK"), "BREACH")

    def test_a_scope_refused_without_a_policy_errno_fails(self) -> None:
        # EINVAL (22) is not a policy refusal.
        self.assert_fail(clean().replace("scope_net:REFUSED:13", "scope_net:REFUSED:22"), "not the policy's")

    def test_a_scope_refused_without_its_label_deny_fails(self) -> None:
        serial = clean().replace(f"LABEL:DENY label={LABEL} iface=0x{BROWSER:016x} method=1\n", "")
        self.assert_fail(serial, "scope_browser: no LABEL:DENY")

    def test_denials_of_the_wrong_interfaces_do_not_count(self) -> None:
        # webd answering EACCES itself, with two unrelated denials of the label, is not the kernel's gate.
        serial = clean().replace(f"0x{NET:016x}", f"0x{VIEW:016x}").replace(f"0x{BROWSER:016x}", f"0x{VIEW:016x}")
        self.assert_fail(serial, "scope_net: no LABEL:DENY")
        self.assert_fail(serial, "other LABEL:DENY")

    def test_an_extra_deny_of_the_label_fails(self) -> None:
        serial = clean().replace(
            "WEBDATTACK:UP:PASS",
            f"LABEL:DENY label={LABEL} file=/home/user/.secret access=stat\nWEBDATTACK:UP:PASS",
        )
        self.assert_fail(serial, "other LABEL:DENY")

    def test_a_time_stamped_deny_is_read(self) -> None:
        self.assert_pass(clean().replace("LABEL:DENY", "[  12.345] LABEL:DENY"))

    def test_a_repeated_probe_fails(self) -> None:
        # An allowed scope followed by a refused one must not pass on the last line.
        serial = clean().replace(
            "WEBDATTACK:PROBE:scope_net:REFUSED:13",
            "WEBDATTACK:PROBE:scope_net:OK\nWEBDATTACK:PROBE:scope_net:REFUSED:13",
        )
        self.assert_fail(serial, "reported 2 times")

    def test_a_webd_restart_mid_run_fails(self) -> None:
        self.assert_fail(clean().replace("WEBDATTACK:DONE:PASS", "WEBD:UP:PASS\nWEBDATTACK:DONE:PASS"), "WEBD:UP:PASS 2 times")

    def test_a_suppressed_trace_fails(self) -> None:
        self.assert_fail(clean() + "LABEL:SUPPRESSED 4\n", "dropped lines")

    def test_the_cap_at_the_wrong_count_fails(self) -> None:
        cap = f"view_cap:REFUSED:{aj.ENOSPC}:{aj.VIEWS_PER_CLIENT}"
        self.assert_fail(clean().replace(cap, "view_cap:REFUSED:28:3"), "view_cap")
        self.assert_fail(clean().replace(cap, "view_cap:OK"), "view_cap")

    def test_a_dead_webd_fails_alive_after(self) -> None:
        self.assert_fail(clean().replace("alive_after:OK", "alive_after:NOTRUNNING"), "alive_after")

    def test_a_missing_probe_fails(self) -> None:
        self.assert_fail(clean().replace("WEBDATTACK:PROBE:flood_alive:OK\n", ""), "flood_alive")

    def test_no_done_or_quit_fails(self) -> None:
        self.assert_fail(clean().replace("WEBDATTACK:DONE:PASS\n", ""), "DONE")
        self.assert_fail(clean().replace("WEBDATTACK:QUIT:PASS\n", ""), "QUIT")


class AgreementTests(unittest.TestCase):
    """The judge's numbers are the code's."""

    def test_the_view_cap_is_the_services_and_the_probes(self) -> None:
        limits = (ROOT / "xui-app" / "crates" / "webd" / "src" / "limits.rs").read_text(encoding="utf-8")
        attack = (ROOT / "xui-app" / "crates" / "webd-client" / "src" / "attack.rs").read_text(encoding="utf-8")
        for source in (limits, attack):
            found = re.search(r"pub const VIEWS_PER_CLIENT: usize = (\d+);", source)
            self.assertIsNotNone(found)
            self.assertEqual(int(found.group(1)), aj.VIEWS_PER_CLIENT)

    def test_the_scope_ids_are_distinct_interfaces_of_webd(self) -> None:
        self.assertEqual(len({NET, BROWSER, VIEW}), 3)
        # The ids the real run printed (2026-10-22).
        self.assertEqual((NET, BROWSER), (0xF313574271CEBA59, 0x2C352157F6AB1380))


if __name__ == "__main__":
    unittest.main()
