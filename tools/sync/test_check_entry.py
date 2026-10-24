#!/usr/bin/env python3
"""Self-test: check_entry passes on the tree and fails when it should."""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import check_entry  # noqa: E402


def tree(files):
    tmp = tempfile.TemporaryDirectory()
    root = Path(tmp.name)
    for rel, text in files.items():
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        (root / rel).write_text(text)
    return tmp, root


class CheckEntry(unittest.TestCase):
    def test_tree_is_clean(self):
        self.assertEqual(check_entry.problems(check_entry.scan()), [])

    def test_every_exit_shape_counts(self):
        tmp, root = tree(
            {
                "a.rs": "        iretq\n",
                "b.rs": "        iretq                 /* resume */\n",
                "c.rs": 'concat!("pop rax\\n", " iretq")\n',
                "d.rs": '"cli\\n swapgs\\n sysretq\\n"\n',
            }
        )
        with tmp:
            self.assertEqual(
                check_entry.scan(root), {"a.rs": 1, "b.rs": 1, "c.rs": 1, "d.rs": 1}
            )

    def test_a_new_stub_fails(self):
        tmp, root = tree({"dev/new.rs": 'global_asm!("new_isr:\\n call handler\\n iretq\\n");\n'})
        with tmp:
            found = check_entry.scan(root)
            self.assertEqual(found, {"dev/new.rs": 1})
            self.assertTrue(any("dev/new.rs" in p for p in check_entry.exit_problems(found)))

    def test_prose_comments_and_tests_do_not_count(self):
        tmp, root = tree(
            {
                "tests/t.rs": "        iretq\n",
                "a.rs": "// the stub ends in iretq\n/* sysretq makes rcx\n   the RIP, iretq */\n",
            }
        )
        with tmp:
            self.assertEqual(check_entry.scan(root), {})

    def test_a_stub_calling_another_callee_fails(self):
        real = (check_entry.KERNEL / "arch" / "linux.rs").read_text(encoding="utf-8")
        swapped = real.replace("call linux_dispatch_gate", "call linux_dispatch")
        self.assertNotEqual(real, swapped)
        tmp, root = tree({"arch/linux.rs": swapped})
        with tmp:
            problems = check_entry.call_problems(root)
            self.assertIn(
                "arch/linux.rs: a stub calls linux_dispatch, which is not a hooked callee", problems
            )

    def test_an_unhooked_callee_fails(self):
        tmp, root = tree(
            {
                "task/schedule.rs": "pub extern \"C\" fn schedule(rsp: u64) -> u64 {\n"
                "    // sync::entry::enter(cs, rip) in a comment does not count\n    rsp\n}\n"
            }
        )
        with tmp:
            self.assertIn(
                "task/schedule.rs: fn schedule does not call sync::entry::enter",
                check_entry.call_problems(root),
            )

    def test_an_unlisted_opt_out_fails(self):
        tmp, root = tree({"dev/irq.rs": "irq_gate!(disk_gate => disk_handler, window_safe);\n"})
        with tmp:
            problems = check_entry.opt_out_problems(check_entry.opt_outs(root))
            self.assertIn(
                "dev/irq.rs: disk_gate takes no entry lock: list it in OPT_OUTS with the reason, "
                "or lock it",
                problems,
            )


if __name__ == "__main__":
    unittest.main()
