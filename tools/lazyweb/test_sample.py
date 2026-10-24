"""Tests for `sample.py`: the register parser and the summary."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import sample  # noqa: E402

KERNEL = """CPU#0
RAX=0000000000000016 RBX=ffff800000316840
RIP=ffff80000007fe6e RFL=00000297 [--S-APC] CPL=0 II=0 A20=1 SMM=0 HLT=0
CR0=80010013 CR2=000000000069f000 CR3=0000000005f04000 CR4=00040620
"""
USER = KERNEL.replace("ffff80000007fe6e", "0000000000799488").replace("CPL=0", "CPL=3")


class ParseTests(unittest.TestCase):
    def test_a_kernel_snapshot(self):
        self.assertEqual(sample.parse_registers(KERNEL), (0, 0x5F04000, 0xFFFF80000007FE6E))

    def test_a_user_snapshot(self):
        self.assertEqual(sample.parse_registers(USER), (3, 0x5F04000, 0x799488))

    def test_a_snapshot_with_a_missing_field_is_refused(self):
        self.assertIsNone(sample.parse_registers(KERNEL.replace("CR3=", "CRX=")))
        self.assertIsNone(sample.parse_registers(""))


class StackTests(unittest.TestCase):
    def test_the_monitors_quadwords(self):
        text = ("0000700000001000: 0x0000000000412345 0x0000000000000000\r\n"
                "0000700000001010: 0x00007fffffffe000 0x00000000079b2ab0\r\n")
        self.assertEqual(sample.parse_words(text), [0x412345, 0, 0x7FFFFFFFE000, 0x79B2AB0])
        self.assertEqual(sample.parse_words(""), [])

    def test_a_stack_is_kept_only_while_the_same_process_runs_in_user_mode(self):
        class Monitor:
            def __init__(self, answers):
                self.answers = list(answers)

            def execute(self, command, **arguments):
                return self.answers.pop(0)

        user = USER.replace("CR0=", "RSP=00007ffffffff000 CR0=")
        moved = user.replace("RSP=00007ffffffff000", "RSP=00007fffffffe000")
        stack = "00007ffffffff000: 0x0000000000412345 0x0000000000000001\r\n"
        self.assertEqual(sample.snapshot(Monitor([user, stack, user]), True),
                         ((3, 0x5F04000, 0x799488), [0x412345, 1]))
        self.assertEqual(sample.snapshot(Monitor([user, stack, moved]), True)[1], [0x412345, 1])
        self.assertEqual(sample.snapshot(Monitor([user, stack, KERNEL]), True)[1], [])
        self.assertEqual(sample.snapshot(Monitor([KERNEL]), True), ((0, 0x5F04000, 0xFFFF80000007FE6E), None))
        self.assertEqual(sample.snapshot(Monitor([user]), False), ((3, 0x5F04000, 0x799488), None))

    def test_a_paused_sample_reads_one_instant_and_always_lets_the_guest_go(self):
        class Monitor:
            def __init__(self, answers):
                self.answers, self.commands = list(answers), []

            def execute(self, command, **arguments):
                self.commands.append(arguments.get("command-line", command).split(" ")[0])
                if command in ("stop", "cont"):
                    return {}
                answer = self.answers.pop(0)
                if isinstance(answer, Exception):
                    raise answer
                return answer

        user = USER.replace("CR0=", "RSP=00007ffffffff000 CR0=")
        stack = "00007ffffffff000: 0x0000000000412345 0x0000000000000001\r\n"
        monitor = Monitor([user, stack])
        self.assertEqual(sample.paused_snapshot(monitor, 2), ((3, 0x5F04000, 0x799488), [0x412345, 1]))
        self.assertEqual(monitor.commands, ["stop", "info", "x", "cont"], "registers and stack of one instant")
        monitor = Monitor([KERNEL])
        self.assertEqual(sample.paused_snapshot(monitor, 2)[1], None)
        self.assertEqual(monitor.commands[-1], "cont")
        monitor = Monitor([user, RuntimeError("unmapped")])
        self.assertEqual(sample.paused_snapshot(monitor, 2)[1], [])
        self.assertEqual(monitor.commands[-1], "cont")
        monitor = Monitor([OSError("gone")])
        with self.assertRaises(OSError):
            sample.paused_snapshot(monitor, 2)
        self.assertEqual(monitor.commands[-1], "cont", "a failed read still resumes the guest")

    def test_a_recording_with_and_without_stacks_loads(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "samples.tsv"
            path.write_text("0.1\t0\t0000000000001000\tffff800000001000\n"
                            "0.2\t3\t0000000000001000\t0000000000401000\t412345,0,79b2ab0\n", encoding="utf-8")
            self.assertEqual(sample.load(path)[1], (0.2, 3, 0x1000, 0x401000))
            rows = sample.load_stacks(path)
            self.assertEqual(rows[0][4], [])
            self.assertEqual(rows[1][4], [0x412345, 0, 0x79B2AB0])


class SummaryTests(unittest.TestCase):
    def rows(self):
        idle = [(i * 0.03, 0, 0x5F04000, 0xFFFF80000007FE6E) for i in range(8)]
        busy = [(1.0 + i * 0.03, 3, 0x14B10000, 0x1234) for i in range(2)]
        return idle + busy

    def test_the_busiest_space_is_first_and_percentages_add_up(self):
        lines = sample.summarize(self.rows())
        self.assertIn("10 samples", lines[0])
        self.assertIn("cpl=0 cr3=5f04000: 8 (80%)", lines[1])
        self.assertIn("cpl=3 cr3=14b10000: 2 (20%)", lines[2])

    def test_one_process_lists_only_its_own_addresses(self):
        lines = sample.summarize(self.rows(), cr3=0x14B10000)
        self.assertTrue(any("0000000000001234  2" in line for line in lines))
        self.assertFalse(any("ffff80000007fe6e" in line for line in lines))

    def test_an_empty_recording_says_so(self):
        self.assertEqual(sample.summarize([]), ["no samples"])

    def test_load_skips_lines_that_are_not_samples(self):
        import tempfile

        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "s.tsv"
            path.write_text("0.030\t3\t0000000014b10000\t0000000000001234\nnot a sample\n", encoding="utf-8")
            self.assertEqual(sample.load(path), [(0.03, 3, 0x14B10000, 0x1234)])


class SymbolizeTests(unittest.TestCase):
    def test_the_address_given_to_addr2line_is_the_runtime_address_less_the_base(self):
        seen = {}

        def fake(command, **_):
            seen["command"] = command

            class Done:
                stdout = "main\nmain.cpp:3\nfoo\nfoo.cpp:9\n"

            return Done()

        names = sample.symbolize(Path("prog"), [0x1010, 0x2020], 0x1000, tool="addr2line", run=fake)
        self.assertEqual(seen["command"][-2:], ["0x10", "0x1020"])
        self.assertEqual(names, {0x1010: "main (main.cpp:3)", 0x2020: "foo (foo.cpp:9)"})

    def test_static_pies_are_not_rebased_by_default(self):
        self.assertEqual(sample.DEFAULT_BASE, 0)

    def test_without_a_tool_or_addresses_there_is_nothing(self):
        self.assertEqual(sample.symbolize(Path("prog"), [], 0, tool="addr2line"), {})


if __name__ == "__main__":
    unittest.main()
