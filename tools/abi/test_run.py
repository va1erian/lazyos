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


if __name__ == "__main__":
    unittest.main()
