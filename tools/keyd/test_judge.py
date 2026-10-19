#!/usr/bin/env python3
"""The keyd secrets judge fails when it should (run.py's verdicts)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import keyd_judge as judge  # noqa: E402


def log(name: str, loaded: int | None = None, details: dict | None = None) -> str:
    """A serial log that satisfies boot `name`, with `details` overriding steps."""
    expect = judge.BOOTS[name]
    count = expect["loaded"] if loaded is None else loaded
    lines = [f"KEYD:SECRETS:PASS count={count} file=/conf/svc/keyd/secrets"]
    for index, (step, detail) in enumerate(expect["steps"]):
        detail = (details or {}).get(index, detail)
        lines.append(f"TERM:OUT:KEYD:T:{step}:PASS:{detail}")
    lines.append(f"KEYD:APPSECRETS:PASS count={expect['app_loaded']}")
    for app, verb, outcome in expect["apps"]:
        lines.append(f"TERM:OUT:KEYD:APP:{app}:{verb}:{outcome}")
    lines.append(f"KEYD:SWEEP:PASS removed={expect['sweep']}")
    return "\n".join(lines) + "\n"


NODE = ("f", "600", "0", "0", "100", "1", "abc")


class BootTest(unittest.TestCase):
    def test_a_good_boot_passes(self):
        for name in judge.BOOTS:
            self.assertEqual(judge.judge_boot(name, log(name)), [], name)

    def test_the_wrong_number_of_loaded_secrets_fails(self):
        failures = judge.judge_boot("second", log("second", loaded=0))
        self.assertTrue(any("loaded 0" in f for f in failures))

    def test_a_pmk_that_answered_fails(self):
        text = log("first").replace("pmk_denied:PASS:EPERM", "pmk_denied:FAIL:ok")
        self.assertTrue(judge.judge_boot("first", text))

    def test_a_secret_lost_over_a_reboot_fails(self):
        failures = judge.judge_boot("second", log("second", details={0: ""}))
        self.assertTrue(any("user_list" in f for f in failures))

    def test_a_missing_step_or_a_refused_file_fails(self):
        text = "\n".join(log("first").splitlines()[:-1]) + "\n"
        self.assertTrue(judge.judge_boot("first", text))
        refused = log("second").replace(
            "KEYD:SECRETS:PASS count=2", "KEYD:SECRETS:FAIL reason=damaged moved_aside=true")
        self.assertTrue(judge.judge_boot("second", refused))

    def test_a_write_failure_or_panic_fails(self):
        for bad in ("KEYD:SECRETS:WRITE:FAIL errno=5", "panic"):
            self.assertTrue(judge.judge_boot("third", log("third") + bad + "\n"), bad)


class AppSecretsTest(unittest.TestCase):
    def test_a_probe_that_read_another_apps_secret_fails(self):
        # `probeb` must not find `token` on its first run; "ok" instead of
        # ENOENT means it saw probea's.
        text = log("first").replace("probeb:first:PASS:ENOENT", "probeb:first:PASS:ok")
        self.assertTrue(any("probe apps printed" in f for f in judge.judge_boot("first", text)))

    def test_a_secret_lost_over_a_reboot_fails(self):
        text = log("second").replace("survived", "MISMATCH")
        self.assertTrue(any("probe apps printed" in f for f in judge.judge_boot("second", text)))

    def test_an_unlabelled_caller_that_was_served_fails(self):
        text = log("first").replace("app_denied:PASS:EPERM", "app_denied:PASS:ok")
        self.assertTrue(any("app_denied" in f for f in judge.judge_boot("first", text)))

    def test_a_missing_or_wrong_sweep_fails(self):
        text = log("second").replace("KEYD:SWEEP:PASS removed=1", "KEYD:SWEEP:PASS removed=0")
        self.assertTrue(any("sweep" in f for f in judge.judge_boot("second", text)))
        text = log("second").replace("KEYD:SWEEP:PASS removed=1\n", "")
        self.assertTrue(any("sweep" in f for f in judge.judge_boot("second", text)))
        self.assertTrue(any("sweep" in f for f in judge.judge_boot(
            "first", log("first").replace("removed=0", "removed=1"))))

    def test_the_wrong_number_of_loaded_app_secrets_fails(self):
        text = log("third").replace("APPSECRETS:PASS count=1", "APPSECRETS:PASS count=2")
        self.assertTrue(any("loaded 2 app secrets" in f for f in judge.judge_boot("third", text)))
        text = log("third").replace("APPSECRETS:PASS count=1", "APPSECRETS:FAIL reason=x")
        self.assertTrue(judge.judge_boot("third", text))

    def test_a_secret_value_in_the_log_fails(self):
        for value in judge.SECRET_VALUES:
            self.assertTrue(judge.judge_boot("third", log("third") + f"x {value}\n"), value)

    def test_the_file_is_checked(self):
        tree = {"/conf/svc/keyd/appsecrets": NODE}
        good = b"LZAPSECR\x01\x00\x02\x00" + bytes(range(60, 124))
        key = bytes(range(100, 132))
        self.assertEqual(judge.judge_app_file(tree, good, key), [])
        self.assertTrue(judge.judge_app_file({}, good, key))
        self.assertTrue(judge.judge_app_file(
            {"/conf/svc/keyd/appsecrets": ("f", "644", "0", "0", "9", "1", "x")}, good, key))
        self.assertTrue(judge.judge_app_file(tree, b"LZSECRT1" + good[8:], key))
        self.assertTrue(judge.judge_app_file(tree, b"LZAPSECR\x02\x00\x02\x00" + good[12:], key))
        self.assertTrue(judge.judge_app_file(tree, b"LZAPSECR\x01\x00\x01\x00" + good[12:], key))
        self.assertTrue(judge.judge_app_file(tree, good + b"s3cret-probea", key))
        self.assertTrue(judge.judge_app_file(tree, good + b"org.keyd.probea", key))
        self.assertTrue(judge.judge_app_file(tree, good + key, key))


class FilesTest(unittest.TestCase):
    TREE = {"/conf/svc/keyd/secrets": NODE, "/conf/svc/keyd/machine.key": NODE,
            "/conf/svc/keyd": ("d", "700", "0", "0", "-", "-", "-")}
    SECRETS = b"LZSECRT1" + bytes(range(64))
    KEY = bytes(range(100, 132))

    def test_sealed_root_files_pass(self):
        self.assertEqual(judge.judge_files(self.TREE, self.SECRETS, self.KEY), [])

    def test_a_readable_file_fails(self):
        tree = dict(self.TREE, **{"/conf/svc/keyd/machine.key": ("f", "644", "0", "0", "32", "1", "x")})
        self.assertTrue(judge.judge_files(tree, self.SECRETS, self.KEY))
        tree = dict(self.TREE, **{"/conf/svc/keyd/secrets": ("f", "600", "1000", "0", "9", "1", "x")})
        self.assertTrue(judge.judge_files(tree, self.SECRETS, self.KEY))

    def test_a_secret_in_the_clear_or_a_missing_file_fails(self):
        self.assertTrue(judge.judge_files(self.TREE, self.SECRETS + b"correct horse", self.KEY))
        self.assertTrue(judge.judge_files({}, self.SECRETS, self.KEY))
        self.assertTrue(judge.judge_files(self.TREE, b"plain", self.KEY))
        self.assertTrue(judge.judge_files(self.TREE, self.SECRETS, b"short"))


if __name__ == "__main__":
    unittest.main()
