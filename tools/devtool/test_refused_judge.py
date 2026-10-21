#!/usr/bin/env python3
"""Self-test of tools/devtool/refused_judge.py: it accepts a log that shows every
refusal and fails, naming the missing one, when a refusal or the toast is absent
or when something was created."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import refused_judge as rj  # noqa: E402

IDS = rj.interface_ids(rj.MANIFEST.read_text(encoding="utf-8"))


def deny(name: str, label: str = rj.LABEL) -> str:
    return f"LABEL:DENY label={label} iface={IDS[name]:#018x} method=1"


def good_log() -> str:
    lines = ["PKGD:PROVISION:DONE installed=19 upgraded=0 kept=0 failed=0 free=1"]
    lines += [deny(name) for name in rj.KERNEL_REFUSED]
    lines += [
        "PKGD:INSTALL:FAIL applications may not install or remove other applications",
        "PLAYGROUND:RUN:1:EXIT:0:0123456789abcdef",
        "SHELL:NOTIFY:POST owner=os.lazy.playground id=1 level=Info title=pg toast",
        "PLAYGROUND:RUN:2:EXIT:0:fedcba9876543210",
    ]
    return "\n".join(lines) + "\n"


class Judge(unittest.TestCase):
    def test_a_log_with_every_refusal_passes(self):
        self.assertEqual(rj.judge_serial(good_log(), IDS), [])

    def test_the_ids_come_from_the_manifest(self):
        for name in rj.KERNEL_REFUSED:
            self.assertIn(name, IDS)

    def test_each_missing_kernel_refusal_is_named(self):
        for name in rj.KERNEL_REFUSED:
            text = "\n".join(l for l in good_log().splitlines() if l != deny(name)) + "\n"
            problems = rj.judge_serial(text, IDS)
            self.assertEqual(len(problems), 1, (name, problems))
            self.assertIn(name, problems[0])

    def test_a_denial_of_another_label_does_not_count(self):
        other = good_log().replace(deny("os.lazy.mount.v1"), deny("os.lazy.mount.v1", "tool:os.lazy.terminal"))
        problems = rj.judge_serial(other, IDS)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("os.lazy.mount.v1", problems[0])

    def test_pkgds_own_refusal_is_required(self):
        text = good_log().replace("PKGD:INSTALL:FAIL applications may not install or remove other applications\n", "")
        problems = rj.judge_serial(text, IDS)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("pkgd did not refuse", problems[0])

    def test_anything_created_fails(self):
        for line in (
            "ACCOUNTS:CREATE:PASS user=pgevil uid=1002 admin=1 by=1000",
            "MOUNTD:START pgevil kind=ftp pid=9",
            "PKGD:INSTALL:PASS os.lazy.x",
            "PKGD:REMOVE:PASS os.lazy.playground",
        ):
            problems = rj.judge_serial(good_log() + line + "\n", IDS)
            self.assertEqual(len(problems), 1, (line, problems))
            self.assertIn(line.split()[0], problems[0])

    def test_the_toast_must_be_posted_and_not_denied(self):
        text = good_log().replace("SHELL:NOTIFY:POST", "SHELL:NOTIFY:OTHER")
        self.assertTrue(any("no SHELL:NOTIFY:POST" in p for p in rj.judge_serial(text, IDS)))
        text = good_log() + "SHELL:NOTIFY:DENY why=not-an-app\n"
        self.assertTrue(any("toast was refused" in p for p in rj.judge_serial(text, IDS)))

    def test_both_script_runs_must_finish(self):
        text = good_log().replace("PLAYGROUND:RUN:2:EXIT:0:fedcba9876543210\n", "")
        self.assertTrue(any("two script runs" in p for p in rj.judge_serial(text, IDS)))

    def test_a_log_that_dropped_lines_proves_nothing(self):
        self.assertTrue(rj.judge_serial(good_log() + "SERIAL:DROPPED bytes=10 total=30\n", IDS))


if __name__ == "__main__":
    unittest.main()
