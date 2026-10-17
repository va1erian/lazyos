#!/usr/bin/env python3
"""The cache check's judge fails when it should (issue #892)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from cachecheck import judge  # noqa: E402

GOOD = "WEBKIT:W4:PASS\n"


def second(cache_fds: int) -> str:
    return f"WEBKIT:CACHE:FDS:total=40:cache={cache_fds}\nWEBKIT:W4:PASS\n"


class JudgeTest(unittest.TestCase):
    def test_a_bounded_count_with_the_cache_used_passes(self) -> None:
        self.assertEqual(judge(GOOD, second(2), served_again=0, count=300, max_fds=8), [])

    def test_one_descriptor_per_entry_fails(self) -> None:
        problems = judge(GOOD, second(300), served_again=0, count=300, max_fds=8)
        self.assertTrue(any("descriptors below the cache" in p for p in problems), problems)

    def test_a_cache_that_was_not_used_proves_nothing(self) -> None:
        problems = judge(GOOD, second(0), served_again=300, count=300, max_fds=8)
        self.assertTrue(any("not used" in p for p in problems), problems)

    def test_a_missing_count_or_a_failed_run_fails(self) -> None:
        self.assertTrue(judge(GOOD, GOOD, served_again=0, count=300, max_fds=8))
        self.assertTrue(judge("", second(0), served_again=0, count=300, max_fds=8))


if __name__ == "__main__":
    unittest.main()
