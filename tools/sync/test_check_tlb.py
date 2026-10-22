#!/usr/bin/env python3
"""Self-test: check_tlb passes on the tree and fails when it should."""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import check_tlb  # noqa: E402


class CheckTlb(unittest.TestCase):
    def test_tree_is_clean(self):
        self.assertEqual(check_tlb.problems(check_tlb.scan()), [])

    def test_a_new_bare_flush_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "mem").mkdir()
            (root / "mem" / "new.rs").write_text(
                "fn f() { x86_64::instructions::tlb::flush(va); }\n"
            )
            found = check_tlb.scan(root)
            self.assertEqual(found, {"mem/new.rs": 1})
            self.assertTrue(check_tlb.problems(found))

    def test_comments_and_tests_do_not_count(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "tests").mkdir()
            (root / "tests" / "t.rs").write_text("x86_64::instructions::tlb::flush_all();\n")
            (root / "a.rs").write_text("// instructions::tlb::flush(va)\n")
            self.assertEqual(check_tlb.scan(root), {})

    def test_a_moved_site_is_reported(self):
        self.assertTrue(check_tlb.problems({}))


if __name__ == "__main__":
    unittest.main()
