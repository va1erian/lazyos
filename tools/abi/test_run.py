"""Tests for the exit-code policy of the ABI bench (`run.py`), offline."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run  # noqa: E402


def rows(**statuses: str) -> list[dict]:
    return [{"fixture": n, "status": s, "detail": ""} for n, s in statuses.items()]


class GateTests(unittest.TestCase):
    def test_fail_and_not_run_gate(self) -> None:
        self.assertEqual(run.failing_rows(rows(hello="fail", b="not-run", c="pass")), ["hello", "b"])

    def test_skip_and_unavailable_do_not_gate(self) -> None:
        self.assertEqual(run.failing_rows(rows(a="skip", b="unavailable", c="pass")), [])


class ClassifyTests(unittest.TestCase):
    def test_a_row_may_report_numbers_before_its_marker(self) -> None:
        log = "BIGAPP:threads=3\nABI:bigapp:threads=9:mem=4:fds=5:PASS\n"
        self.assertEqual(run.classify("bigapp", log), ("pass", "threads=9 mem=4 fds=5"))

    def test_a_plain_marker_has_no_detail(self) -> None:
        self.assertEqual(run.classify("hello", "ABI:hello:PASS\n"), ("pass", ""))

    def test_numbers_do_not_make_another_row_pass(self) -> None:
        self.assertEqual(run.classify("thread", "ABI:bigapp:threads=9:PASS\n")[0], "not-run")

    def test_a_failure_still_names_its_reason(self) -> None:
        log = "ABI:bigapp:FAIL:mmap failed with errno 5\n"
        self.assertEqual(run.classify("bigapp", log), ("fail", "mmap failed with errno 5"))


if __name__ == "__main__":
    unittest.main()
