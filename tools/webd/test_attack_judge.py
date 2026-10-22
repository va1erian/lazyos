#!/usr/bin/env python3
"""The malicious-client attack judge (`attackjudge.py`) fails when it should: a clean run passes, and
each kind of breach is caught."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import attackjudge as aj  # noqa: E402

LABEL = aj.LABEL


def clean() -> str:
    """A serial log of a run where every probe did what it must."""
    return "\n".join(
        [
            f"LABEL:DENY label={LABEL} iface=0xf313574271ceba59 method=1",
            f"LABEL:DENY label={LABEL} iface=0x2c352157f6ab1380 method=1",
            "WEBDATTACK:UP:PASS",
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

    def test_a_scope_allowed_is_a_breach(self) -> None:
        self.assert_fail(clean().replace("scope_net:REFUSED:13", "scope_net:OK"), "BREACH")

    def test_a_scope_refused_without_a_policy_errno_fails(self) -> None:
        # EINVAL (22) is not a policy refusal.
        self.assert_fail(clean().replace("scope_net:REFUSED:13", "scope_net:REFUSED:22"), "not the policy's")

    def test_a_scope_refused_without_a_label_deny_fails(self) -> None:
        # Drop one of the two scope denies: the refusal was not shown to be the kernel's.
        serial = clean().replace(
            f"LABEL:DENY label={LABEL} iface=0x2c352157f6ab1380 method=1\n", ""
        )
        self.assert_fail(serial, "not the kernel's policy")

    def test_an_extra_deny_of_the_label_fails(self) -> None:
        serial = clean().replace(
            "WEBDATTACK:UP:PASS",
            f"LABEL:DENY label={LABEL} file=/home/user/.secret access=stat\nWEBDATTACK:UP:PASS",
        )
        self.assert_fail(serial, "more than the")

    def test_the_cap_at_the_wrong_count_fails(self) -> None:
        self.assert_fail(
            clean().replace(f"view_cap:REFUSED:{aj.ENOSPC}:{aj.VIEWS_PER_CLIENT}", "view_cap:REFUSED:28:3"),
            "view_cap",
        )

    def test_the_cap_allowed_fails(self) -> None:
        self.assert_fail(clean().replace(f"view_cap:REFUSED:{aj.ENOSPC}:{aj.VIEWS_PER_CLIENT}", "view_cap:OK"), "view_cap")

    def test_a_dead_webd_fails_alive_after(self) -> None:
        self.assert_fail(clean().replace("alive_after:OK", "alive_after:NOTRUNNING"), "alive_after")

    def test_a_missing_probe_fails(self) -> None:
        self.assert_fail(clean().replace("WEBDATTACK:PROBE:flood_alive:OK\n", ""), "flood_alive")

    def test_no_done_fails(self) -> None:
        self.assert_fail(clean().replace("WEBDATTACK:DONE:PASS\n", ""), "DONE")

    def test_no_quit_fails(self) -> None:
        self.assert_fail(clean().replace("WEBDATTACK:QUIT:PASS\n", ""), "QUIT")


if __name__ == "__main__":
    unittest.main()
