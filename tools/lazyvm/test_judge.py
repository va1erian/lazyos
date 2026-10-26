#!/usr/bin/env python3
"""Self-test of `tools/lazyvm/judge.py` and the result files of `run.py` (no QEMU, no build).

Run: python tools/lazyvm/test_judge.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge  # noqa: E402
import run  # noqa: E402

LINE = judge.encode(120, ["/system/lazyvm/bin/lazyvm", "-cp", "a b", ""])


def serial(*lines: str) -> str:
    """A serial log with kernel noise and CR/LF line ends around `lines`."""
    noise = "mem: reclaimed address space 0x2dfc000\r\n"
    return noise + "".join(f"{line}\r\n" for line in lines) + noise


def data(n: int, stream: str, seq: int, payload: bytes) -> str:
    return f"LAZYVM:DATA:{n}:{stream}:{seq}:{payload.hex()}"


class ParseTests(unittest.TestCase):
    def test_the_list_line_hexes_every_argument_empty_ones_too(self) -> None:
        self.assertEqual(LINE.split(" ")[0], "120")
        self.assertEqual(bytes.fromhex(LINE.split(" ")[3]), b"a b")
        self.assertEqual(LINE.split(" ")[4], "")

    def test_output_comes_back_byte_exact_from_chunks_in_order(self) -> None:
        out = b"line one\n\ttab\r\nno newline"
        log = serial("LAZYVM:INIT:1", f"LAZYVM:CMD:1:{LINE.encode().hex()}",
                     data(1, "out", 1, out[10:]), data(1, "out", 0, out[:10]),
                     data(1, "err", 0, b"boom\n"), f"LAZYVM:END:1:1:{len(out)}:5:0",
                     "LAZYVM:DONE")
        run_ = judge.parse(log)
        self.assertEqual(run_.errors, [])
        self.assertTrue(run_.done)
        self.assertEqual(run_.count, 1)
        result = run_.results[1]
        self.assertEqual((result.stdout, result.stderr, result.exit), (out, b"boom\n", 1))
        self.assertEqual(result.line, LINE.encode())

    def test_a_lost_chunk_or_a_missing_end_is_an_error(self) -> None:
        lost = judge.parse(serial(data(1, "out", 1, b"x"), "LAZYVM:END:1:0:2:0:0"))
        self.assertTrue(any("chunks" in error for error in lost.errors))
        self.assertTrue(any("bytes" in error for error in lost.errors))
        cut = judge.parse(serial(data(1, "out", 0, b"x")))
        self.assertIn("command 1: no END line", cut.errors)

    def test_a_line_cut_by_the_end_of_the_log_is_reported_not_raised(self) -> None:
        parsed = judge.parse("LAZYVM:DATA:1:out:0:6")
        self.assertTrue(parsed.errors)

    def test_timeouts_and_failures_are_read(self) -> None:
        parsed = judge.parse(serial("LAZYVM:END:1:124:0:0:1", "LAZYVM:FAIL:cannot read"))
        self.assertTrue(parsed.results[1].timed_out)
        self.assertEqual(parsed.failure, "cannot read")


class ResultFileTests(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())

    def test_each_command_gets_its_three_files(self) -> None:
        parsed = judge.parse(serial("LAZYVM:INIT:1", f"LAZYVM:CMD:1:{LINE.encode().hex()}",
                                    data(1, "out", 0, b"hi\n"), "LAZYVM:END:1:3:3:0:0",
                                    "LAZYVM:DONE"))
        self.assertEqual(run.write_results(parsed, [LINE], self.dir), [])
        self.assertEqual((self.dir / "1.stdout").read_bytes(), b"hi\n")
        self.assertEqual((self.dir / "1.stderr").read_bytes(), b"")
        self.assertEqual((self.dir / "1.exit").read_bytes(), b"3\n")

    def test_an_image_built_for_other_commands_is_refused(self) -> None:
        other = judge.encode(120, ["/system/lazyvm/bin/lazyvm", "Other"])
        parsed = judge.parse(serial("LAZYVM:INIT:1", f"LAZYVM:CMD:1:{other.encode().hex()}",
                                    "LAZYVM:END:1:0:0:0:0", "LAZYVM:DONE"))
        problems = run.write_results(parsed, [LINE], self.dir)
        self.assertTrue(any("another one" in problem for problem in problems))
        self.assertFalse((self.dir / "1.exit").exists())

    def test_a_boot_that_never_reached_the_fixture_fails(self) -> None:
        problems = run.write_results(judge.parse("kernel panic\n"), [LINE], self.dir)
        self.assertIn(f"command 1 did not finish: {LINE}", problems)

    def test_the_command_file_splits_like_a_shell(self) -> None:
        path = self.dir / "commands.txt"
        path.write_text("# comment\n\n/x/lazyvm -cp /t \"a b\" ''\n", encoding="utf-8")
        self.assertEqual(run.read_commands(path), [["/x/lazyvm", "-cp", "/t", "a b", ""]])


if __name__ == "__main__":
    unittest.main()
