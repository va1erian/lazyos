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


class OomTests(unittest.TestCase):
    def test_an_oom_kill_gates_unless_expected(self) -> None:
        table = rows(bigapp="oom", hello="pass")
        self.assertEqual(run.failing_rows(table), ["bigapp"])
        self.assertEqual(run.failing_rows(table, expect_oom=True), [])

    def test_expect_oom_does_not_excuse_other_failures(self) -> None:
        self.assertEqual(run.failing_rows(rows(a="fail", b="not-run"), expect_oom=True), ["a", "b"])


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

    def test_an_oom_kill_reads_as_sigkill(self) -> None:
        log = "BIGAPP:mem=900\noom: killed abi-init (1000): 1536 MiB mapped, 0 frames\n"
        self.assertEqual(run.classify("bigapp", log), ("oom", "killed by SIGKILL (oom)"))

    def test_a_marker_beats_an_oom_kill_of_something_else(self) -> None:
        log = "oom: killed other (1000): 9 MiB mapped, 0 frames\nABI:bigapp:PASS\n"
        self.assertEqual(run.classify("bigapp", log)[0], "pass")

    def test_a_page_fault_death_names_its_signal(self) -> None:
        log = "user: task 7 killed by page fault (address Ok(VirtAddr(0x1)), rip 0x2), status 139\n"
        self.assertEqual(run.classify("bigapp", log), ("fail", "killed by SIGSEGV (page fault)"))


if __name__ == "__main__":
    unittest.main()
